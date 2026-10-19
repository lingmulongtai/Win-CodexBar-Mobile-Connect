import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import type { DesktopPeerStatus } from "../../../types/bridge";
const state = vi.hoisted(() => ({ status: null as DesktopPeerStatus | null }));
const api = vi.hoisted(() => ({ saveDesktopPeer: vi.fn(), selectDesktopPeer: vi.fn(), removeDesktopPeer: vi.fn() }));
vi.mock("../../../lib/tauri", () => api);
vi.mock("../../../hooks/useDesktopPeerStatus", () => ({ useDesktopPeerStatus: () => state.status }));
vi.mock("../../../hooks/useLocale", () => ({ useLocale: () => ({ t: (key: string) => key }) }));
import DesktopPeerSettings from "./DesktopPeerSettings";

beforeEach(() => {
  vi.clearAllMocks();
  state.status = { selectedId: "home", sourceId: "peer:home:1", name: "Home", connections: [{ id: "home", name: "Home", address: "100.64.1.2:43825" }], receiving: false, lastReceived: 100, connected: true, error: null, storageError: null };
  api.saveDesktopPeer.mockResolvedValue(state.status);
  api.selectDesktopPeer.mockResolvedValue(state.status);
  api.removeDesktopPeer.mockResolvedValue(state.status);
});

it("saves the explicitly entered PC, masks the code and clears it only after success", async () => {
  render(<DesktopPeerSettings />);
  const code = screen.getByLabelText("DesktopPeerCode");
  expect(code).toHaveAttribute("type", "password");
  fireEvent.change(screen.getByLabelText("DesktopPeerName"), { target: { value: "Work" } });
  fireEvent.change(code, { target: { value: "synthetic-private-code" } });
  expect(api.saveDesktopPeer).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "DesktopPeerConnect" }));
  await waitFor(() => expect(api.saveDesktopPeer).toHaveBeenCalledWith("Work", "synthetic-private-code"));
  await waitFor(() => expect(code).toHaveValue(""));
  expect(screen.queryByText("synthetic-private-code")).not.toBeInTheDocument();
});

it("can return to local acquisition and remove a connected PC directly", async () => {
  render(<DesktopPeerSettings />);
  fireEvent.change(screen.getByRole("combobox"), { target: { value: "" } });
  await waitFor(() => expect(api.selectDesktopPeer).toHaveBeenCalledWith(null));
  await waitFor(() => expect(screen.getByRole("button", { name: "DesktopPeerRemove: Home" })).toBeEnabled());
  fireEvent.click(screen.getByRole("button", { name: "DesktopPeerRemove: Home" }));
  await waitFor(() => expect(api.removeDesktopPeer).toHaveBeenCalledWith("home"));
  expect(api.saveDesktopPeer).not.toHaveBeenCalled();
});

it("blocks writes when the original connection store cannot be read", () => {
  state.status!.storageError = "Original settings preserved";
  render(<DesktopPeerSettings />);
  expect(screen.getByRole("alert")).toHaveTextContent("Original settings preserved");
  expect(screen.getByRole("combobox")).toBeDisabled();
  expect(screen.getByRole("button", { name: "DesktopPeerRemove: Home" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "DesktopPeerConnect" })).toBeDisabled();
});
