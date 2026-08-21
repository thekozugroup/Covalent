//! Choosing the peer address this node tells other devices to dial.
//!
//! # Why this is not a one-liner
//!
//! The address a node advertises ends up inside a mutually signed pairing
//! transcript and is the address a phone will actually dial. Getting it wrong is
//! worse than having no answer at all: a wrong address produces a device that
//! appears in the discovered list, accepts a tap, and then times out with
//! nothing for the user to act on. So this module answers only when it can
//! answer correctly, and otherwise refuses with a reason specific enough to fix.
//!
//! Three cases have to be told apart, and only the first is easy.
//!
//! * **Host networking.** The interfaces the process can see are the machine's
//!   real interfaces. Pick the private LAN address and be done.
//! * **A container on a bridge network.** This is the *default* on Unraid, and
//!   the interface the process sees is `172.17.x.x` on Docker's own bridge. That
//!   address is reachable from the host and from sibling containers and from
//!   nothing else — emphatically not from the phone the user is holding. The
//!   peer port is published on the host, so the address peers must dial is the
//!   *host's* LAN address, which the container cannot observe. Detected and
//!   refused by name, with the override to set.
//! * **Several plausible interfaces.** VLANs, a second NIC, a VPN. Any choice
//!   might be wrong, so the choice is at least made deterministically — the same
//!   interface every restart — because an advertised address that changes across
//!   restarts invalidates transcripts peers already signed.
//!
//! Public addresses are never chosen automatically. This is a private backup
//! product; publishing a globally routable endpoint because it happened to be
//! the only interface would leak the node's existence to the internet. An
//! operator who genuinely wants that sets it explicitly.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Why automatic selection declined to produce an address.
///
/// Each variant exists because it needs a different sentence in front of the
/// user; collapsing them into one "no usable address" would recreate the exact
/// dead end this module is meant to remove.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddressRefusal {
    /// Running in a container whose only private addresses are on a container
    /// bridge. The host publishes the port; the container cannot see the host.
    ContainerBridgeOnly {
        /// The address that was seen and rejected, named so the operator can
        /// recognise it in the log.
        observed: IpAddr,
    },
    /// Interfaces exist but none carries a private, non-loopback address.
    NoPrivateInterface,
}

impl AddressRefusal {
    /// A message that names the cause and the exact remedy.
    ///
    /// Deliberately mentions the environment variable and the Unraid template
    /// field by name. Bridge networking is the default on Unraid, so this is the
    /// common path rather than an edge case, and a message that only says what
    /// went wrong would leave the majority of users stuck.
    #[must_use]
    pub fn operator_guidance(&self) -> String {
        match self {
            Self::ContainerBridgeOnly { observed } => format!(
                "Covalent is running in a container on a bridge network, so the only address it \
                 can see ({observed}) belongs to the container and is not reachable from your \
                 phone or laptop. Set COVALENT_ADVERTISED_PEER_ADDRESS to this server's LAN \
                 address and peer port, for example 192.168.1.50:8787. In Unraid this is the \
                 \"Address other devices dial\" field in the Covalent template."
            ),
            Self::NoPrivateInterface => "Covalent could not find a private network address to \
                 advertise to other devices. Set COVALENT_ADVERTISED_PEER_ADDRESS to this \
                 server's LAN address and peer port, for example 192.168.1.50:8787. In Unraid \
                 this is the \"Address other devices dial\" field in the Covalent template."
                .to_owned(),
        }
    }
}

/// How useful one interface address is for reaching this node from a LAN.
///
/// Ordering is the selection policy: `Ord` sorts lowest first, so the best
/// candidate is the minimum.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum AddressClass {
    /// RFC 1918 or IPv6 unique-local. What a phone on the same network dials.
    PrivateLan,
    /// Tailscale's CGNAT range. Correct, but only for peers already on the
    /// tailnet, so it loses to a plain LAN address when both exist.
    Tailnet,
    /// A container bridge address. Never selected; tracked so the refusal can
    /// name the specific case instead of shrugging.
    ContainerBridge,
}

/// Classifies one address, or `None` if it must never be advertised.
fn classify(address: IpAddr) -> Option<AddressClass> {
    match address {
        IpAddr::V4(address) => classify_v4(address),
        IpAddr::V6(address) => classify_v6(address),
    }
}

fn classify_v4(address: Ipv4Addr) -> Option<AddressClass> {
    if address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_broadcast()
        || address.is_multicast()
        || address.is_documentation()
    {
        return None;
    }
    let [first, second, ..] = address.octets();
    // Tailscale hands out 100.64.0.0/10, the carrier-grade NAT range.
    if first == 100 && (64..128).contains(&second) {
        return Some(AddressClass::Tailnet);
    }
    // Docker's default bridge is 172.17.0.0/16 and user-defined bridges take
    // the rest of 172.16/12. That range overlaps a legitimate private LAN, so
    // this classification alone never refuses anything — it is combined with an
    // actual container marker in `select_advertised_address`.
    if first == 172 && (16..32).contains(&second) {
        return Some(AddressClass::ContainerBridge);
    }
    if address.is_private() {
        return Some(AddressClass::PrivateLan);
    }
    None
}

fn classify_v6(address: Ipv6Addr) -> Option<AddressClass> {
    if address.is_loopback() || address.is_unspecified() || address.is_multicast() {
        return None;
    }
    let segments = address.segments();
    // fe80::/10, link-local: not routable off the link and not stable.
    if segments[0] & 0xffc0 == 0xfe80 {
        return None;
    }
    // Tailscale's IPv6 allocation, fd7a:115c:a1e0::/48.
    if segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0 {
        return Some(AddressClass::Tailnet);
    }
    // fc00::/7, unique local.
    if segments[0] & 0xfe00 == 0xfc00 {
        return Some(AddressClass::PrivateLan);
    }
    None
}

/// Picks the address to advertise from a list of observed interface addresses.
///
/// `in_container` gates the bridge refusal: the same `172.16/12` address is a
/// perfectly good LAN address on a bare-metal host and a dead end inside a
/// container, and nothing about the address itself distinguishes the two.
///
/// Selection is deterministic. Candidates sort by class, then IPv4 before IPv6,
/// then by the address bytes, so a machine with several interfaces advertises
/// the same one on every restart. That matters more than picking the *best* one:
/// peers hold signed transcripts naming the address, and an advertised address
/// that changes across restarts invalidates them.
pub fn select_advertised_address(
    observed: &[IpAddr],
    in_container: bool,
) -> Result<IpAddr, AddressRefusal> {
    let mut candidates: Vec<(AddressClass, bool, IpAddr)> = observed
        .iter()
        .filter_map(|&address| classify(address).map(|class| (class, address.is_ipv6(), address)))
        .collect();
    candidates.sort();

    let bridge_only = candidates
        .iter()
        .find(|&&(class, _, _)| class == AddressClass::ContainerBridge);

    for &(class, _, address) in &candidates {
        match class {
            AddressClass::PrivateLan | AddressClass::Tailnet => return Ok(address),
            // A bridge address is usable when this is not a container: it is
            // then simply a LAN on 172.16/12, which is a legitimate choice.
            AddressClass::ContainerBridge if !in_container => return Ok(address),
            AddressClass::ContainerBridge => {}
        }
    }

    Err(match bridge_only {
        Some(&(_, _, observed)) => AddressRefusal::ContainerBridgeOnly { observed },
        None => AddressRefusal::NoPrivateInterface,
    })
}

/// Reads the host's interface addresses.
///
/// Failure to enumerate is reported as an empty list rather than an error: the
/// caller's next step is the same refusal either way, and that refusal already
/// names the override.
#[must_use]
pub fn observed_interface_addresses() -> Vec<IpAddr> {
    if_addrs::get_if_addrs().map_or_else(
        |_| Vec::new(),
        |interfaces| {
            interfaces
                .into_iter()
                .filter(|interface| !interface.is_loopback())
                .map(|interface| interface.ip())
                .collect()
        },
    )
}

/// Best-effort detection of running inside a container.
///
/// Only ever *narrows* what is advertised — a false positive refuses and asks
/// for an explicit address, a false negative advertises a `172.16/12` address
/// that would have been advertised anyway. Neither outcome can produce a wrong
/// address silently, which is why a heuristic is acceptable here.
#[must_use]
pub fn running_in_container() -> bool {
    // Docker writes this marker; Podman and containerd write the second.
    std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists()
        || std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|cgroup| {
            cgroup.contains("/docker/")
                || cgroup.contains("/containerd/")
                || cgroup.contains("/kubepods")
        })
}

/// Resolves the endpoint to advertise, preferring an explicit operator choice.
///
/// The explicit value wins unconditionally and is never second-guessed: an
/// operator behind a reverse proxy, on a VLAN, or on a custom Docker network
/// knows something this process cannot observe.
pub fn resolve_advertised_endpoint(
    bound: SocketAddr,
    configured: Option<SocketAddr>,
    observed: &[IpAddr],
    in_container: bool,
) -> Result<SocketAddr, AddressRefusal> {
    if let Some(configured) = configured
        && !configured.ip().is_unspecified()
    {
        let port = if configured.port() == 0 {
            bound.port()
        } else {
            configured.port()
        };
        return Ok(SocketAddr::new(configured.ip(), port));
    }
    // A concrete non-loopback bind is already the answer: the operator pinned
    // the socket to one interface, so that interface is the intended one.
    if !bound.ip().is_unspecified() && !bound.ip().is_loopback() {
        return Ok(bound);
    }
    let address = select_advertised_address(observed, in_container)?;
    Ok(SocketAddr::new(address, bound.port()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("address")
    }

    #[test]
    fn a_private_lan_address_is_preferred_over_a_tailnet_one() {
        assert_eq!(
            select_advertised_address(&[ip("100.101.102.103"), ip("192.168.1.50")], false),
            Ok(ip("192.168.1.50")),
            "a phone on the same LAN must not be sent to the tailnet address"
        );
        // Tailnet is still chosen when it is all there is, because a tailnet
        // peer really can dial it.
        assert_eq!(
            select_advertised_address(&[ip("100.101.102.103")], false),
            Ok(ip("100.101.102.103"))
        );
    }

    #[test]
    fn addresses_that_must_never_be_advertised_are_never_chosen() {
        for address in [
            "127.0.0.1",
            "169.254.10.1",
            "0.0.0.0",
            "8.8.8.8",
            "203.0.113.7",
            "::1",
            "fe80::1",
            "2606:4700::1111",
        ] {
            assert_eq!(
                select_advertised_address(&[ip(address)], false),
                Err(AddressRefusal::NoPrivateInterface),
                "{address} must never be advertised automatically"
            );
        }
        // A public address is refused even alongside nothing else, which is the
        // point: this product does not publish itself to the internet by
        // accident.
        assert_eq!(
            select_advertised_address(&[ip("8.8.8.8"), ip("2606:4700::1111")], false),
            Err(AddressRefusal::NoPrivateInterface)
        );
    }

    #[test]
    fn a_container_on_a_bridge_refuses_by_name_instead_of_advertising_a_dead_end() {
        let refusal = select_advertised_address(&[ip("172.17.0.2")], true)
            .expect_err("a bridge address is unreachable from the LAN");
        assert_eq!(
            refusal,
            AddressRefusal::ContainerBridgeOnly {
                observed: ip("172.17.0.2")
            }
        );

        // The message has to do real work, so assert it names both the observed
        // address and the exact override rather than merely being non-empty.
        let guidance = refusal.operator_guidance();
        assert!(guidance.contains("172.17.0.2"), "{guidance}");
        assert!(
            guidance.contains("COVALENT_ADVERTISED_PEER_ADDRESS"),
            "{guidance}"
        );
        assert!(guidance.contains("Unraid"), "{guidance}");

        // The identical address on a bare-metal host is a legitimate LAN and is
        // accepted, which is why the container marker is required and the
        // address alone is not enough to refuse on.
        assert_eq!(
            select_advertised_address(&[ip("172.17.0.2")], false),
            Ok(ip("172.17.0.2"))
        );

        // A container with host networking sees the real LAN address and works.
        assert_eq!(
            select_advertised_address(&[ip("172.17.0.2"), ip("192.168.1.50")], true),
            Ok(ip("192.168.1.50"))
        );
    }

    #[test]
    fn selection_is_stable_across_restarts_whatever_the_enumeration_order() {
        let forwards = [
            ip("192.168.1.50"),
            ip("10.0.0.7"),
            ip("fd00::5"),
            ip("100.101.102.103"),
        ];
        let mut backwards = forwards;
        backwards.reverse();
        let first = select_advertised_address(&forwards, false).expect("selected");
        assert_eq!(
            first,
            select_advertised_address(&backwards, false).expect("selected"),
            "interface order must not change what peers were told to dial"
        );
        assert_eq!(
            first,
            ip("10.0.0.7"),
            "the tie-break is the address itself, so the choice is reproducible"
        );
        // IPv4 sorts ahead of IPv6 within a class, so a dual-stack host does not
        // flip between families across restarts.
        assert_eq!(
            select_advertised_address(&[ip("fd00::5"), ip("10.0.0.7")], false),
            Ok(ip("10.0.0.7"))
        );
    }

    #[test]
    fn an_explicit_override_wins_and_an_empty_host_is_not_silently_accepted() {
        let bound: SocketAddr = "0.0.0.0:8787".parse().expect("bound");

        // The override is taken verbatim, including a public address, because an
        // operator who types one means it.
        assert_eq!(
            resolve_advertised_endpoint(
                bound,
                Some("203.0.113.7:9000".parse().expect("override")),
                &[],
                true,
            ),
            Ok("203.0.113.7:9000".parse().expect("expected"))
        );
        // A zero port inherits the bound port, so the operator only has to name
        // the address.
        assert_eq!(
            resolve_advertised_endpoint(
                bound,
                Some("192.168.1.50:0".parse().expect("override")),
                &[],
                true,
            ),
            Ok("192.168.1.50:8787".parse().expect("expected"))
        );
        // An unspecified override is not an override; auto-detection runs and,
        // finding nothing, refuses rather than advertising 0.0.0.0.
        assert_eq!(
            resolve_advertised_endpoint(
                bound,
                Some("0.0.0.0:8787".parse().expect("override")),
                &[],
                false,
            ),
            Err(AddressRefusal::NoPrivateInterface)
        );
    }

    #[test]
    fn a_pinned_bind_is_its_own_answer_but_loopback_is_not() {
        // Binding to one interface states the intent outright.
        assert_eq!(
            resolve_advertised_endpoint(
                "192.168.1.50:8787".parse().expect("bound"),
                None,
                &[],
                false,
            ),
            Ok("192.168.1.50:8787".parse().expect("expected"))
        );
        // A loopback bind must not be advertised: no other device can dial it.
        // This is the default in `NodeRuntimeConfig`, so getting it wrong would
        // hand every embedded app an unusable endpoint.
        assert_eq!(
            resolve_advertised_endpoint(
                "127.0.0.1:8787".parse().expect("bound"),
                None,
                &[ip("192.168.1.50")],
                false,
            ),
            Ok("192.168.1.50:8787".parse().expect("expected"))
        );
    }

    #[test]
    fn enumeration_never_panics_on_the_host_running_these_tests() {
        // Not an assertion about this machine's networking — only that reading
        // the interface list is safe and that everything it returns is
        // classifiable without surprises.
        let observed = observed_interface_addresses();
        for address in &observed {
            assert!(!address.is_loopback(), "loopback is filtered at the source");
        }
        let _ = select_advertised_address(&observed, running_in_container());
    }
}
