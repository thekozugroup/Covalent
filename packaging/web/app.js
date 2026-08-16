const deviceName = document.querySelector("[data-device-name]");
const stateText = document.querySelector("[data-state]");
const discovery = document.querySelector("[data-discovery]");
const refresh = document.querySelector("[data-refresh]");

async function loadStatus() {
  refresh.disabled = true;
  stateText.textContent = "Connecting to the local service.";
  try {
    const response = await fetch("/api/v1/status", {
      headers: { Accept: "application/json" },
      cache: "no-store",
    });
    if (!response.ok) throw new Error(`status ${response.status}`);
    const status = await response.json();
    deviceName.textContent = status.deviceName;
    stateText.textContent = `Service state: ${status.state}. Protocol ${status.protocolVersion}.`;
    discovery.textContent = status.lanDiscovery ? "On" : "Off";
  } catch (_error) {
    deviceName.textContent = "Node unavailable";
    stateText.textContent = "The local Covalent service did not respond. Check the container or daemon logs.";
    discovery.textContent = "Unknown";
  } finally {
    refresh.disabled = false;
  }
}

refresh.addEventListener("click", loadStatus);
loadStatus();
