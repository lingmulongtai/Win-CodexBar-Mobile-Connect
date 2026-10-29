import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { SettingsSnapshot } from "../../../types/bridge";
import DiscordTab from "./DiscordTab";
import { openExternalUrl, resolveCodexPet } from "../../../lib/tauri";

vi.mock("../../../lib/tauri", () => ({ resolveCodexPet: vi.fn(), openExternalUrl: vi.fn() }));

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
  it("previews selectable services and saves priority and field choices", () => {
    const set = vi.fn();
    render(<DiscordTab settings={{} as SettingsSnapshot} set={set} saving={false} />);
    fireEvent.change(screen.getByRole("combobox", { name: "DiscordPreviewProvider" }), { target: { value: "claude" } });
    expect(screen.getByText(/Claude Code · Claude Sonnet/)).toBeInTheDocument();
    fireEvent.change(screen.getByRole("combobox", { name: "DiscordPriority" }), { target: { value: "codex" } });
    expect(set).toHaveBeenCalledWith(expect.objectContaining({ discordPresenceOptions: expect.objectContaining({ priority: "codex" }) }));
    fireEvent.click(screen.getByRole("checkbox", { name: "DiscordShowModel" }));
    expect(set).toHaveBeenCalledWith(expect.objectContaining({ discordPresenceOptions: expect.objectContaining({ showModel: false }) }));
  });

  it("resolves a Codex Pets link before saving its public poster", async () => {
    const set = vi.fn();
    vi.mocked(resolveCodexPet).mockResolvedValueOnce("https://codex-pets.net/assets/pets/v/123/astro-burn/poster.webp");
    render(<DiscordTab settings={{} as SettingsSnapshot} set={set} saving={false} />);
    const input = screen.getByRole("textbox", { name: "DiscordPetUrl" });
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "https://codex-pets.net/#/pets/astro-burn" } });
    fireEvent.blur(input);
    await waitFor(() => expect(set).toHaveBeenCalledWith({ discordPresenceOptions: { imageUrl: "https://codex-pets.net/assets/pets/v/123/astro-burn/poster.webp" } }));
    expect(resolveCodexPet).toHaveBeenCalledWith("https://codex-pets.net/#/pets/astro-burn");
  });

  it("reports a failed link without saving it", async () => {
    const set = vi.fn();
    vi.mocked(resolveCodexPet).mockRejectedValueOnce(new Error("not found"));
    render(<DiscordTab settings={{} as SettingsSnapshot} set={set} saving={false} />);
    const input = screen.getByRole("textbox", { name: "DiscordPetUrl" });
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "https://codex-pets.net/share/missing" } });
    fireEvent.blur(input);
    expect(await screen.findByRole("alert")).toHaveTextContent("DiscordPetInvalid");
    expect(set).not.toHaveBeenCalled();
  });

  it("shows the selected layout in the preview", () => {
    const set = vi.fn();
    const settings = { discordPresenceOptions: { format: "modelFirst" } } as SettingsSnapshot;
    const { rerender } = render(<DiscordTab settings={settings} set={set} saving={false} />);
    expect(screen.getByText(/GPT-5.5 · Codex/)).toBeInTheDocument();
    rerender(<DiscordTab settings={{ discordPresenceOptions: { format: "service" } } as SettingsSnapshot} set={set} saving={false} />);
    expect(screen.getByText(/Using Codex/)).toBeInTheDocument();
    expect(screen.queryByText(/GPT-5.5/)).not.toBeInTheDocument();
  });

  it("saves the GitHub URL and lets the user open it from the preview", () => {
    const set = vi.fn();
    vi.mocked(openExternalUrl).mockResolvedValue(undefined);
    const settings = { discordPresenceOptions: { githubUrl: "https://github.com/example/project" } } as SettingsSnapshot;
    render(<DiscordTab settings={settings} set={set} saving={false} />);
    fireEvent.click(screen.getByRole("button", { name: "View on GitHub" }));
    expect(openExternalUrl).toHaveBeenCalledWith("https://github.com/example/project");
    const input = screen.getByRole("textbox", { name: "DiscordGithubUrl" });
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "https://github.com/example/other" } });
    fireEvent.blur(input);
    expect(set).toHaveBeenCalledWith({ discordPresenceOptions: { githubUrl: "https://github.com/example/other" } });
  });

  it("does not save a non-GitHub button destination", () => {
    const set = vi.fn();
    render(<DiscordTab settings={{} as SettingsSnapshot} set={set} saving={false} />);
    const input = screen.getByRole("textbox", { name: "DiscordGithubUrl" });
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "https://github.com.attacker.example/repo" } });
    fireEvent.blur(input);
    expect(screen.getByRole("alert")).toHaveTextContent("DiscordGithubInvalid");
    expect(set).not.toHaveBeenCalled();
  });

});
