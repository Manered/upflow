import * as Dialog from "@radix-ui/react-dialog";
import * as Switch from "@radix-ui/react-switch";
import { LoaderCircle, Minus, Monitor, Moon, Plus, Sun, X } from "lucide-react";
import { useRef, useState, type FocusEvent } from "react";
import { desktop, failure, type Settings } from "./native";
import { Button } from "./ui";

const themes = [
  { value: "system", name: "System", icon: Monitor },
  { value: "light", name: "Light", icon: Sun },
  { value: "dark", name: "Dark", icon: Moon },
] as const;

export function SettingsDialog({
  value,
  outputFocus,
  onClose,
  onSave,
  returnFocus,
}: {
  value: Settings;
  outputFocus: boolean;
  onClose: () => void;
  onSave: (settings: Settings) => void;
  returnFocus: () => void;
}) {
  const [open, setOpen] = useState(true);
  const [draft, setDraft] = useState(value);
  const [threads, setThreads] = useState(String(value.threads));
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [writing, setWriting] = useState(false);
  const saving = useRef(false);
  const outputButton = useRef<HTMLButtonElement>(null);

  async function persist(next: Settings) {
    if (saving.current) return;
    saving.current = true;
    setBusy(true);
    setError("");
    setWriting(true);
    const previous = draft;
    setDraft(next);
    try {
      const saved = await desktop.saveSettings(next);
      setDraft(saved);
      setThreads(String(saved.threads));
      onSave(saved);
    } catch (error) {
      setDraft(previous);
      setThreads(String(previous.threads));
      setError(failure(error).message);
    } finally {
      saving.current = false;
      setBusy(false);
      setWriting(false);
    }
  }

  function update<K extends keyof Settings>(key: K, val: Settings[K]) {
    void persist({ ...draft, [key]: val });
  }

  async function choose(kind: "outputDirectory" | "ffmpegPath") {
    if (saving.current) return;
    saving.current = true;
    setBusy(true);
    setError("");
    try {
      const path = await (kind === "outputDirectory" ? desktop.chooseDirectory() : desktop.chooseFFmpeg());
      saving.current = false;
      if (typeof path === "string") {
        await persist({
          ...draft,
          [kind]: path,
          useSourceDirectory: kind === "outputDirectory" ? false : draft.useSourceDirectory,
        });
      }
    } catch (error) {
      setError(failure(error).message);
    } finally {
      saving.current = false;
      setBusy(false);
    }
  }

  function commitThreads(event: FocusEvent<HTMLInputElement>) {
    const count = Number(event.currentTarget.value);
    if (!Number.isInteger(count) || count < 1 || count > 256) {
      setThreads(String(draft.threads));
      setError("Enter a thread count from 1 to 256.");
    } else if (count !== draft.threads) update("threads", count);
  }

  function changeThreads(step: number) {
    update("threads", Math.min(256, Math.max(1, (Number(threads) || 16) + step)));
  }

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(next) => {
        if (!saving.current) setOpen(next);
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="modal-overlay" />
        <Dialog.Content
          className="modal settings-modal"
          aria-describedby={undefined}
          onOpenAutoFocus={(event) => {
            if (outputFocus) {
              event.preventDefault();
              outputButton.current?.focus();
            }
          }}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            returnFocus();
            onClose();
          }}
        >
          <div className="modal-header">
            <Dialog.Title>Settings</Dialog.Title>
            {writing && (
              <span className="settings-saving" role="status">
                <LoaderCircle className="spinner" />
                Saving
              </span>
            )}
            <Dialog.Close asChild>
              <Button variant="icon" aria-label="Close settings" disabled={busy}>
                <X />
              </Button>
            </Dialog.Close>
          </div>
          <div className="settings-scroll">
            <fieldset className="theme-field" disabled={busy}>
              <legend>Appearance</legend>
              <div className="theme-options">
                {themes.map(({ value, name, icon: Icon }) => (
                  <label key={value}>
                    <input
                      type="radio"
                      name="theme"
                      value={value}
                      checked={draft.theme === value}
                      onChange={() => update("theme", value)}
                    />
                    <span>
                      <Icon />
                      {name}
                    </span>
                  </label>
                ))}
              </div>
            </fieldset>
            <section className="settings-group" aria-labelledby="export-settings">
              <h3 id="export-settings">Export</h3>
              <div className="setting-row setting-row-with-value">
                <div className="setting-copy">
                  <span className="setting-label">Output folder</span>
                  <span
                    className="setting-value"
                    title={draft.useSourceDirectory ? undefined : (draft.outputDirectory ?? undefined)}
                  >
                    {draft.useSourceDirectory ? "Same as source" : draft.outputDirectory}
                  </span>
                </div>
                <div className="setting-actions">
                  {!draft.useSourceDirectory && (
                    <Button
                      variant="ghost"
                      className="compact"
                      disabled={busy}
                      onClick={() => update("useSourceDirectory", true)}
                    >
                      Use source
                    </Button>
                  )}
                  <Button
                    ref={outputButton}
                    className="compact"
                    disabled={busy}
                    onClick={() => void choose("outputDirectory")}
                  >
                    Browse
                  </Button>
                </div>
              </div>
              <Toggle
                id="auto-open"
                label="Open output folder when finished"
                checked={draft.openFolderAfterConversion}
                disabled={busy}
                onChange={(checked) => update("openFolderAfterConversion", checked)}
              />
            </section>
            <section className="settings-group" aria-labelledby="encoder-settings">
              <h3 id="encoder-settings">Encoder</h3>
              <div className="setting-row">
                <label className="setting-label" htmlFor="threads">
                  CPU threads
                </label>
                <div className="stepper">
                  <Button
                    variant="icon"
                    aria-label="Use one fewer thread"
                    onClick={() => changeThreads(-1)}
                    disabled={busy || Number(threads) <= 1}
                  >
                    <Minus />
                  </Button>
                  <input
                    id="threads"
                    type="number"
                    min={1}
                    max={256}
                    required
                    value={threads}
                    disabled={busy}
                    onChange={(event) => setThreads(event.target.value)}
                    onBlur={commitThreads}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") event.currentTarget.blur();
                    }}
                  />
                  <Button
                    variant="icon"
                    aria-label="Use one more thread"
                    onClick={() => changeThreads(1)}
                    disabled={busy || Number(threads) >= 256}
                  >
                    <Plus />
                  </Button>
                </div>
              </div>
              <div className="setting-row setting-row-with-value">
                <div className="setting-copy">
                  <span className="setting-label">FFmpeg executable</span>
                  <span className="setting-value" title={draft.ffmpegPath ?? undefined}>
                    {draft.ffmpegPath ?? "Bundled FFmpeg"}
                  </span>
                </div>
                <div className="setting-actions">
                  {draft.ffmpegPath && (
                    <Button
                      variant="ghost"
                      className="compact"
                      disabled={busy}
                      onClick={() => update("ffmpegPath", null)}
                    >
                      Use bundled
                    </Button>
                  )}
                  <Button className="compact" disabled={busy} onClick={() => void choose("ffmpegPath")}>
                    Browse
                  </Button>
                </div>
              </div>
              <Toggle
                id="keep-logs"
                label="Keep encoding logs"
                checked={draft.keepLogs}
                disabled={busy}
                onChange={(checked) => update("keepLogs", checked)}
              />
            </section>
            {error && (
              <p className="settings-error" role="alert">
                {error}
              </p>
            )}
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function Toggle({
  id,
  label,
  checked,
  disabled,
  onChange,
}: {
  id: string;
  label: string;
  checked: boolean;
  disabled: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <div className="setting-row">
      <div className="setting-copy">
        <label className="setting-label" htmlFor={id}>
          {label}
        </label>
      </div>
      <Switch.Root
        id={id}
        className="switch"
        checked={checked}
        disabled={disabled}
        onCheckedChange={onChange}
      >
        <Switch.Thumb />
      </Switch.Root>
    </div>
  );
}
