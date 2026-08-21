import Foundation
import Security
import Testing

@testable import CovalentShared

/// Proves the certificate pin actually pins — without a server.
///
/// The only coverage this property had was
/// `packagedCaddyTLSUsesEnrolledExactCAAndRejectsWrongCA`, which needs a
/// packaged Caddy, four environment variables and a driver script that
/// nothing in CI has ever invoked. It opened with `guard … else { return }`,
/// and a bare `return` in swift-testing is a pass, so for its whole life it
/// reported success without opening a socket. That test is now honestly
/// skipped when its server is absent; this suite is what stops the skip from
/// being a hole.
///
/// Everything here runs on every `swift test`. The chain is minted per run by
/// `/usr/bin/openssl` rather than checked in, because Apple enforces the
/// 398-day maximum validity for TLS server certificates *even against a
/// custom anchor* — a committed fixture would silently start failing about a
/// year after it was generated. (Measured, not assumed: a 3650-day leaf is
/// refused with "Certificate exceeds maximum temporal validity period", a
/// 397-day leaf is accepted.)
///
/// ## What these tests catch, and the one thing they do not
///
/// Mutation-tested against `PinnedTrust.accepts`:
///
/// | mutation | result |
/// | --- | --- |
/// | `accepts` returns `true` unconditionally | **caught** (2 tests) |
/// | `SecTrustSetPolicies` removed | **caught** (wrong-host test) |
/// | `SecTrustSetAnchorCertificates` removed | **caught** (2 tests) |
/// | `SecTrustSetAnchorCertificatesOnly` removed | **SURVIVES** |
///
/// The survivor is a real limit, stated rather than hidden. Dropping
/// `AnchorCertificatesOnly` turns pinning into "our anchor *plus* every
/// system root", and the only chain that behaves differently under the two
/// settings is one a public CA actually signed — which cannot be minted here
/// and cannot be committed, because it expires. The packaged-Caddy end-to-end
/// test could close it if it presented a publicly-trusted certificate;
/// nothing else in this target can.
@Suite struct PinnedTrustTests {
    @Test func aChainSignedByThePinnedRootIsAccepted() throws {
        let chain = try TLSChainFixture.make()
        let trust = try chain.serverTrust()
        #expect(PinnedTrust.accepts(trust, host: TLSChainFixture.host, anchor: chain.root))
    }

    @Test func aChainSignedByADifferentRootIsRefused() throws {
        let chain = try TLSChainFixture.make()
        let impostor = try TLSChainFixture.make()
        let trust = try chain.serverTrust()
        // Same host, same shape, valid dates — the *only* difference is which
        // root signed it. That is the whole property being pinned.
        #expect(!PinnedTrust.accepts(trust, host: TLSChainFixture.host, anchor: impostor.root))
    }

    @Test func aChainPresentedForAnotherHostIsRefused() throws {
        let chain = try TLSChainFixture.make()
        let trust = try chain.serverTrust()
        #expect(!PinnedTrust.accepts(trust, host: "not-the-server.invalid", anchor: chain.root))
    }

    @Test func pinningTheIntermediateInsteadOfTheRootStillChainsCorrectly() throws {
        let chain = try TLSChainFixture.make()
        let trust = try chain.serverTrust()
        #expect(PinnedTrust.accepts(trust, host: TLSChainFixture.host, anchor: chain.intermediate))
    }

    /// Negative control on the fixture, not on the code under test.
    ///
    /// If the generated chain were somehow acceptable to the system's default
    /// trust store, then "accepted" above would prove nothing about pinning —
    /// it would just be the system saying yes. The chain must be rejected
    /// when no anchor of ours is supplied.
    @Test func theGeneratedChainIsNotTrustedByTheSystemOnItsOwn() throws {
        let chain = try TLSChainFixture.make()
        let trust = try chain.serverTrust()
        #expect(!SecTrustEvaluateWithError(trust, nil))
    }

    /// Negative control on the fixture generator: two invocations must not
    /// produce the same root, or `aChainSignedByADifferentRootIsRefused`
    /// would be comparing a key against itself.
    @Test func twoFixturesDoNotShareARoot() throws {
        let first = try TLSChainFixture.make()
        let second = try TLSChainFixture.make()
        #expect(
            SecCertificateCopyData(first.root) as Data != SecCertificateCopyData(second.root) as Data
        )
    }
}

/// A root -> intermediate -> leaf chain, minted on demand.
///
/// The shape mirrors what the packaged deployment presents: Caddy's local CA
/// issues through an intermediate, and what a person enrols in the app is the
/// root.
private struct TLSChainFixture {
    static let host = "covalent-node.test"

    let root: SecCertificate
    let intermediate: SecCertificate
    let leaf: SecCertificate

    /// Builds the `SecTrust` a URLSession delegate would be handed: the leaf
    /// plus the intermediate the server sends, and no root.
    ///
    /// Deliberately created with a *basic* X.509 policy rather than an SSL
    /// one. If the fixture pre-bound the hostname, `PinnedTrust.accepts`
    /// could delete its own `SecTrustSetPolicies` call and every test here
    /// would still pass — the hostname check would be coming from the
    /// fixture. Starting permissive means the hostname assertions measure the
    /// code under test.
    func serverTrust() throws -> SecTrust {
        var trust: SecTrust?
        let status = SecTrustCreateWithCertificates(
            [leaf, intermediate] as CFArray,
            SecPolicyCreateBasicX509(),
            &trust
        )
        #expect(status == errSecSuccess)
        return try #require(trust)
    }

    static func make() throws -> TLSChainFixture {
        let openssl = URL(fileURLWithPath: "/usr/bin/openssl")
        // Fail rather than skip: this ships with macOS, and a missing one
        // means the environment is wrong, not that the property is untestable.
        #expect(
            FileManager.default.isExecutableFile(atPath: openssl.path),
            "/usr/bin/openssl is required to mint the test certificate chain"
        )

        let directory = FileManager.default.temporaryDirectory
            .appending(path: "covalent-pin-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        func run(_ arguments: [String]) throws {
            let process = Process()
            process.executableURL = openssl
            process.arguments = arguments
            process.currentDirectoryURL = directory
            let errors = Pipe()
            process.standardOutput = FileHandle.nullDevice
            process.standardError = errors
            try process.run()
            let stderr = errors.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            #expect(
                process.terminationStatus == 0,
                "openssl \(arguments.first ?? "") failed: \(String(decoding: stderr, as: UTF8.self))"
            )
        }

        func write(_ text: String, to name: String) throws {
            try Data(text.utf8).write(to: directory.appending(path: name, directoryHint: .notDirectory))
        }

        try write(
            "basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\n",
            to: "intermediate.ext"
        )
        try write(
            """
            basicConstraints=critical,CA:FALSE
            keyUsage=critical,digitalSignature,keyEncipherment
            extendedKeyUsage=serverAuth
            subjectAltName=DNS:\(host)

            """,
            to: "leaf.ext"
        )

        try run([
            "req", "-x509", "-newkey", "rsa:2048", "-sha256", "-days", "3650", "-nodes",
            "-keyout", "root.key", "-out", "root.crt", "-subj", "/CN=Covalent Test Root",
            "-addext", "basicConstraints=critical,CA:TRUE",
            "-addext", "keyUsage=critical,keyCertSign,cRLSign",
        ])
        try run([
            "req", "-newkey", "rsa:2048", "-sha256", "-nodes",
            "-keyout", "intermediate.key", "-out", "intermediate.csr",
            "-subj", "/CN=Covalent Test Intermediate",
        ])
        try run([
            "x509", "-req", "-in", "intermediate.csr", "-CA", "root.crt", "-CAkey", "root.key",
            "-CAcreateserial", "-out", "intermediate.crt", "-days", "1825", "-sha256",
            "-extfile", "intermediate.ext",
        ])
        try run([
            "req", "-newkey", "rsa:2048", "-sha256", "-nodes",
            "-keyout", "leaf.key", "-out", "leaf.csr", "-subj", "/CN=\(host)",
        ])
        // 397 days: one day inside the maximum Apple enforces for TLS server
        // certificates. See the suite comment.
        try run([
            "x509", "-req", "-in", "leaf.csr", "-CA", "intermediate.crt", "-CAkey", "intermediate.key",
            "-CAcreateserial", "-out", "leaf.crt", "-days", "397", "-sha256", "-extfile", "leaf.ext",
        ])

        func certificate(_ name: String) throws -> SecCertificate {
            try run(["x509", "-in", "\(name).crt", "-outform", "DER", "-out", "\(name).der"])
            let der = try Data(contentsOf: directory.appending(path: "\(name).der", directoryHint: .notDirectory))
            return try #require(SecCertificateCreateWithData(nil, der as CFData))
        }

        return TLSChainFixture(
            root: try certificate("root"),
            intermediate: try certificate("intermediate"),
            leaf: try certificate("leaf")
        )
    }
}
