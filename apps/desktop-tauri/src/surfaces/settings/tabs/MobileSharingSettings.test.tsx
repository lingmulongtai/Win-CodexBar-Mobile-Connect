import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
const api = vi.hoisted(() => ({ getMobileSharingStatus: vi.fn(), getMobileNetworkAddresses: vi.fn(), configureMobileSharing: vi.fn(), getMobilePairingCode: vi.fn(), revokeMobilePairing: vi.fn(), setMobileRemoteSettings: vi.fn() }));
const locale = vi.hoisted(() => ({ t: (key: string) => key }));
vi.mock("../../../lib/tauri", () => api);
vi.mock("../../../hooks/useLocale", () => ({ useLocale: () => locale }));
vi.mock("./DesktopPeerSettings", () => ({ default: () => null }));
import MobileSharingSettings from "./MobileSharingSettings";
const running = { enabled: true, running: true, address: "192.168.1.10", port: 43825, error: null, allowRemoteSettings:false };
describe("Android sharing", () => {
  it("keeps remote changes off until explicitly enabled on this PC", async () => {
    api.setMobileRemoteSettings.mockResolvedValue({...running,allowRemoteSettings:true});
    render(<MobileSharingSettings />);
    const toggle=await screen.findByRole("checkbox",{name:"MobileSharingRemoteSettings"});
    await waitFor(()=>expect(toggle).toBeEnabled());
    expect(toggle).not.toBeChecked();
    expect(api.setMobileRemoteSettings).not.toHaveBeenCalled();
    fireEvent.click(toggle);
    await waitFor(()=>expect(api.setMobileRemoteSettings).toHaveBeenCalledWith(true));
    await waitFor(()=>expect(toggle).toBeChecked());
  });
  beforeEach(() => { vi.clearAllMocks(); api.getMobileSharingStatus.mockResolvedValue(running); api.getMobileNetworkAddresses.mockResolvedValue({ tailscaleAddress: "100.74.102.71", lanAddress: running.address }); });
  it("shows an enabled listener failure separately from successful actions", async () => {
    api.getMobileSharingStatus.mockResolvedValue({ ...running, running: false, error: "Listener unavailable" });
    render(<MobileSharingSettings />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Listener unavailable");
    expect(screen.getByRole("status")).toHaveTextContent("MobileSharingPending");
    expect(screen.queryByRole("button", { name: "MobileSharingCopy" })).not.toBeInTheDocument();
  });
  it("selects the PC's Tailscale endpoint without changing the port or pairing key", async () => {
    api.configureMobileSharing.mockResolvedValue({ ...running, address: "100.74.102.71" });
    render(<MobileSharingSettings />);
    const button = await screen.findByRole("button", { name: "MobileSharingTailscale" });
    await waitFor(() => expect(button).toBeEnabled());
    expect(api.configureMobileSharing).not.toHaveBeenCalled();
    fireEvent.click(button);
    await waitFor(() => expect(api.configureMobileSharing).toHaveBeenCalledWith(true, "100.74.102.71", 43825));
    expect(api.revokeMobilePairing).not.toHaveBeenCalled();
    expect(api.getMobilePairingCode).not.toHaveBeenCalled();
  });
  it("keeps manual pairing available when Tailscale is unavailable", async () => {
    api.getMobileNetworkAddresses.mockResolvedValue({ tailscaleAddress: null, lanAddress: running.address });
    render(<MobileSharingSettings />);
    await screen.findByText("MobileSharingTailscaleUnavailable");
    expect(screen.getByRole("button", { name: "MobileSharingTailscale" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "MobileSharingCopy" })).toBeEnabled();
  });
  it("stops using the saved endpoint even when the draft is invalid", async () => {
    api.configureMobileSharing.mockResolvedValue({ ...running, enabled: false, running: false });
    render(<MobileSharingSettings />);
    const toggle = await screen.findByRole("checkbox", { name: "MobileSharingEnable" });
    await waitFor(() => expect(toggle).toBeChecked());
    fireEvent.change(screen.getByLabelText("MobileSharingAddress"), { target: { value: "invalid" } });
    fireEvent.change(screen.getByLabelText("MobileSharingPort"), { target: { value: "" } });
    fireEvent.click(toggle);
    await waitFor(() => expect(api.configureMobileSharing).toHaveBeenCalledWith(false, "192.168.1.10", 43825));
    await waitFor(() => expect(toggle).not.toBeChecked());
  });
  it("never fetches a pairing secret until Copy is clicked", async () => {
    const clipboard = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText: clipboard }, configurable: true });
    api.getMobilePairingCode.mockResolvedValue("synthetic-test-code");
    render(<MobileSharingSettings />);
    await waitFor(() => expect(screen.getByRole("button", { name: "MobileSharingCopy" })).toBeEnabled());
    expect(api.getMobilePairingCode).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "MobileSharingCopy" }));
    await waitFor(() => expect(clipboard).toHaveBeenCalledWith("synthetic-test-code"));
    expect(screen.queryByText("synthetic-test-code")).not.toBeInTheDocument();
  });
});
