import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SettingsSnapshot } from "../../../types/bridge";
import DiscordTab from "./DiscordTab";
import { getDiscordPresenceStatus, openExternalUrl, resolveCodexPet } from "../../../lib/tauri";

vi.mock("../../../lib/tauri", () => ({ resolveCodexPet: vi.fn(), openExternalUrl: vi.fn(), getDiscordPresenceStatus: vi.fn() }));

vi.mock("../../../hooks/useLocale", () => ({
  useLocale: () => ({ t: (key: string) => key }),
}));

describe("DiscordTab", () => {
  beforeEach(() => {
    vi.mocked(getDiscordPresenceStatus).mockResolvedValue({ status: "connected", usage: "available", activity: {
      details: "⚡ Claude Code · claude-sonnet", state: "📊 D 12k", buttons: [{ label: "View on GitHub", url: "https://github.com/example/project" }],
    } });
  });
  it("lets the user opt in to sharing the Codex account lifetime", async () => {
    const set = vi.fn();
    render(<DiscordTab settings={{} as SettingsSnapshot} set={set} saving={false} />);
    await screen.findByText("DiscordUsageAvailable");
    const toggle = screen.getByRole("checkbox", { name: "DiscordShowCodexLifetime" });
    expect(toggle).not.toBeChecked();
    fireEvent.click(toggle);
    expect(set).toHaveBeenCalledWith({ discordPresenceOptions: { showCodexLifetime: true } });
  });

  it("starts disabled and saves an explicit opt-in", async () => {
    const set = vi.fn();
    render(<DiscordTab settings={{} as SettingsSnapshot} set={set} saving={false} />);
    await screen.findByText("DiscordUsageAvailable");
    const toggle = screen.getByRole("checkbox", { name: "DiscordPresenceLabel" });
    expect(toggle).not.toBeChecked();
    expect(screen.getByText("DiscordPresenceHelper")).toBeInTheDocument();
    fireEvent.click(toggle);
    expect(set).toHaveBeenCalledWith({ discordPresenceEnabled: true });
  });

  it("preserves the enabled value, permits turning it off, and disables edits while saving", async () => {
    const set = vi.fn();
    const settings = { discordPresenceEnabled: true } as SettingsSnapshot;
    const { rerender } = render(<DiscordTab settings={settings} set={set} saving={false} />);
    await screen.findByText("DiscordUsageAvailable");
    const toggle = screen.getByRole("checkbox", { name: "DiscordPresenceLabel" });
    expect(toggle).toBeChecked();
    fireEvent.click(toggle);
    expect(set).toHaveBeenCalledWith({ discordPresenceEnabled: false });
    rerender(<DiscordTab settings={settings} set={set} saving />);
    expect(toggle).toBeDisabled();
  });
  it("saves priority and field choices", async () => {
    const set = vi.fn();
    render(<DiscordTab settings={{} as SettingsSnapshot} set={set} saving={false} />);
    await screen.findByText("DiscordUsageAvailable");
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
    await waitFor(() => expect(set).toHaveBeenCalledWith({ discordPresenceOptions: { imageUrl: "https://codex-pets.net/assets/pets/v/123/astro-burn/poster.webp", petUrl: "https://codex-pets.net/#/pets/astro-burn" } }));
    expect(resolveCodexPet).toHaveBeenCalledWith("https://codex-pets.net/#/pets/astro-burn");
  });

  it("lets users choose separate motions for idle and active work", async () => {
    const set = vi.fn();
    const settings = { discordPresenceOptions: { petUrl: "https://codex-pets.net/#/pets/astro-burn" } } as SettingsSnapshot;
    render(<DiscordTab settings={settings} set={set} saving={false} />);
    await screen.findByText("DiscordUsageAvailable");
    fireEvent.change(screen.getByRole("combobox", { name: "DiscordPetIdleMotion" }), { target: { value: "waving" } });
    expect(set).toHaveBeenCalledWith({ discordPresenceOptions: { petIdleMotion: "waving" } });
    fireEvent.change(screen.getByRole("combobox", { name: "DiscordPetActiveMotion" }), { target: { value: "running-left" } });
    expect(set).toHaveBeenCalledWith({ discordPresenceOptions: { petActiveMotion: "running-left" } });
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

  it("shows only the activity accepted by Discord", async () => {
    render(<DiscordTab settings={{} as SettingsSnapshot} set={vi.fn()} saving={false} />);
    expect(await screen.findByText("⚡ Claude Code · claude-sonnet")).toBeInTheDocument();
    expect(screen.getByText("📊 D 12k")).toBeInTheDocument();
    expect(screen.queryByText(/GPT-5.5/)).not.toBeInTheDocument();
    expect(screen.getByText("DiscordPreviewHelp")).toBeInTheDocument();
  });

  it("updates the preview after Discord accepts a new activity", async () => {
    vi.mocked(getDiscordPresenceStatus)
      .mockResolvedValueOnce({ status: "connected", usage: "available", activity: { details: "Codex", state: "📊 D 1k" } })
      .mockResolvedValue({ status: "connected", usage: "available", activity: { details: "Claude Code", state: "📊 D 2k" } });
    render(<DiscordTab settings={{} as SettingsSnapshot} set={vi.fn()} saving={false} />);
    expect(await screen.findByText("Codex")).toBeInTheDocument();
    expect(await screen.findByText("Claude Code", {}, { timeout: 2500 })).toBeInTheDocument();
    expect(screen.getByText("📊 D 2k")).toBeInTheDocument();
  });

  it("reports an unavailable connection without showing stale data", async () => {
    vi.mocked(getDiscordPresenceStatus).mockResolvedValueOnce({ status: "connecting", usage: "unavailable", activity: null });
    render(<DiscordTab settings={{} as SettingsSnapshot} set={vi.fn()} saving={false} />);
    expect(await screen.findByText("DiscordLiveConnecting")).toBeInTheDocument();
    expect(screen.queryByText(/Claude Code/)).not.toBeInTheDocument();
  });

  it("explains that marked calendar totals exclude deleted files", async () => {
    vi.mocked(getDiscordPresenceStatus).mockResolvedValueOnce({ status: "connected", usage: "localOnly", activity: { details: "Codex", state: "📊 D~ 12k" } });
    render(<DiscordTab settings={{} as SettingsSnapshot} set={vi.fn()} saving={false} />);
    expect(await screen.findByText("📊 D~ 12k")).toBeInTheDocument();
    expect(screen.getByText("DiscordUsageLocalOnly")).toBeInTheDocument();
  });

  it("explains when local token data is unavailable", async () => {
    vi.mocked(getDiscordPresenceStatus).mockResolvedValueOnce({ status: "connected", usage: "unavailable", activity: { details: "Claude Code" } });
    render(<DiscordTab settings={{} as SettingsSnapshot} set={vi.fn()} saving={false} />);
    expect(await screen.findByText("DiscordUsageUnavailable")).toBeInTheDocument();
  });

  it("saves the GitHub URL and lets the user open the sent link", async () => {
    const set = vi.fn();
    vi.mocked(openExternalUrl).mockResolvedValue(undefined);
    const settings = { discordPresenceOptions: { githubUrl: "https://github.com/example/project" } } as SettingsSnapshot;
    render(<DiscordTab settings={settings} set={set} saving={false} />);
    fireEvent.click(await screen.findByRole("button", { name: "View on GitHub" }));
    expect(openExternalUrl).toHaveBeenCalledWith("https://github.com/example/project");
    const input = screen.getByRole("textbox", { name: "DiscordGithubUrl" });
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "https://github.com/example/other" } });
    fireEvent.blur(input);
    expect(set).toHaveBeenCalledWith({ discordPresenceOptions: { githubUrl: "https://github.com/example/other" } });
  });

  it("opens the configured GitHub link even while Discord is disconnected", async () => {
    vi.mocked(getDiscordPresenceStatus).mockResolvedValueOnce({ status: "connecting", usage: "unavailable", activity: null });
    vi.mocked(openExternalUrl).mockResolvedValue(undefined);
    const settings = { discordPresenceOptions: { githubUrl: "https://github.com/example/project" } } as SettingsSnapshot;
    render(<DiscordTab settings={settings} set={vi.fn()} saving={false} />);
    await screen.findByText("DiscordLiveConnecting");
    fireEvent.click(screen.getByRole("button", { name: "DiscordGithubTest" }));
    expect(openExternalUrl).toHaveBeenCalledWith("https://github.com/example/project");
  });

  it("does not save a non-GitHub button destination", async () => {
    const set = vi.fn();
    render(<DiscordTab settings={{} as SettingsSnapshot} set={set} saving={false} />);
    await screen.findByText("DiscordUsageAvailable");
    const input = screen.getByRole("textbox", { name: "DiscordGithubUrl" });
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: "https://github.com.attacker.example/repo" } });
    fireEvent.blur(input);
    expect(screen.getByRole("alert")).toHaveTextContent("DiscordGithubInvalid");
    expect(set).not.toHaveBeenCalled();
  });

});
