import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { SettingsSnapshot } from "../../../types/bridge";
import DiscordTab from "./DiscordTab";

vi.mock("../../../hooks/useLocale", () => ({
  useLocale: () => ({ t: (key: string) => key }),
}));

describe("DiscordTab", () => {
  it("starts disabled and saves an explicit opt-in", () => {
    const set = vi.fn();
    render(<DiscordTab settings={{} as SettingsSnapshot} set={set} saving={false} />);
    const toggle = screen.getByRole("checkbox", { name: "DiscordPresenceLabel" });
    expect(toggle).not.toBeChecked();
    expect(screen.getByText("DiscordPresenceHelper")).toBeInTheDocument();
    fireEvent.click(toggle);
    expect(set).toHaveBeenCalledWith({ discordPresenceEnabled: true });
  });

  it("preserves the enabled value, permits turning it off, and disables edits while saving", () => {
    const set = vi.fn();
    const settings = { discordPresenceEnabled: true } as SettingsSnapshot;
    const { rerender } = render(<DiscordTab settings={settings} set={set} saving={false} />);
    const toggle = screen.getByRole("checkbox", { name: "DiscordPresenceLabel" });
    expect(toggle).toBeChecked();
    fireEvent.click(toggle);
    expect(set).toHaveBeenCalledWith({ discordPresenceEnabled: false });
    rerender(<DiscordTab settings={settings} set={set} saving />);
    expect(toggle).toBeDisabled();
  });
});
