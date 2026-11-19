import * as Dialog from "@radix-ui/react-dialog";
import * as Tooltip from "@radix-ui/react-tooltip";
import {
  ArrowRight,
  ArrowUpRight,
  Check,
  CircleAlert,
  CircleSlash,
  FileVideo,
  FolderOpen,
  LoaderCircle,
  Minus,
  Play,
  Plus,
  RotateCcw,
  Settings2,
  Square,
  SquaresSubtract,
  Terminal,
  Upload,
  X,
} from "lucide-react";
import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { fileLabel, fpsLabel, remainingLabel, sizeLabel, timeLabel } from "./format";
import { desktop, defaults, failure, windowApi, type Settings } from "./native";
import { LogDialog, Notifications, useNotifications } from "./feedback";
import { isFinished, useQueue, type Job } from "./queue";
import { SettingsDialog } from "./settings-dialog";
import { Button } from "./ui";

const homeHeadlines = [
  "Patch your TikToks with upflow",
  "Get your clips ready for TikTok",
  "Give your clips a cleaner upload",
  "Bypass TikTok re-encoding with upflow",
  "Bypass TikTok compression with upflow",
];

export function App() {
  const { toasts, notify, dismiss } = useNotifications();
  const [logId, setLogId] = useState<string | null>(null);
  const queue = useQueue(notify, setLogId);
  const [settings, setSettings] = useState(defaults);
  const [settingsOpen, setSettingsOpen] = useState<"general" | "output" | null>(null);
  const [closePrompt, setClosePrompt] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [maximized, setMaximized] = useState(false);
  const [picking, setPicking] = useState(false);
  const [connected, setConnected] = useState(false);
  const pickerOpen = useRef(false);
  const modalOpenRef = useRef(false);
  const settingsButton = useRef<HTMLButtonElement>(null);

  const activeJob = queue.jobs.find(({ status }) => status === "encoding");
  const pendingCount = queue.jobs.filter(
    ({ status }) => status === "pending" || status === "checking" || status === "ready",
  ).length;
  const modalOpen = !!(settingsOpen || logId || closePrompt);
  modalOpenRef.current = modalOpen;

  function windowAction(action: Promise<unknown>) {
    void action.catch((error: unknown) => notify(failure(error).message, "error"));
  }

  function closeRequested() {
    if (queue.runningRef.current) {
      setLogId(null);
      setClosePrompt(true);
    } else {
      windowAction(desktop.confirmClose());
    }
  }

  async function pickVideos() {
    if (pickerOpen.current) return;
    pickerOpen.current = true;
    setPicking(true);
    setDragging(false);
    try {
      const result = await desktop.chooseVideo();
      if (typeof result === "string") queue.add([result]);
      else if (Array.isArray(result)) queue.add(result);
    } catch (error) {
      notify(failure(error).message, "error");
    } finally {
      setPicking(false);
      pickerOpen.current = false;
    }
  }

  // Keep event listeners registered once while giving them the latest callbacks.
  const handlers = useRef({
    update: queue.onUpdate,
    add: queue.add,
    closeRequested,
    pick: pickVideos,
    notify,
  });
  handlers.current = { update: queue.onUpdate, add: queue.add, closeRequested, pick: pickVideos, notify };

  useEffect(() => {
    let mounted = true;
    const update = desktop.onUpdate((value) => handlers.current.update(value));
    const close = desktop.onCloseRequest(() => handlers.current.closeRequested());
    const drag = windowApi.onDragDropEvent(({ payload }) => {
      const allowed = !modalOpenRef.current && !pickerOpen.current;
      setDragging(allowed && (payload.type === "enter" || payload.type === "over"));
      if (allowed && payload.type === "drop") handlers.current.add(payload.paths);
    });
    const resize = windowApi.onResized(() => {
      void windowApi
        .isMaximized()
        .then(setMaximized)
        .catch(() => {});
    });
    const handleShortcut = (event: KeyboardEvent) => {
      if (event.ctrlKey && event.key.toLowerCase() === "o" && !modalOpenRef.current) {
        event.preventDefault();
        void handlers.current.pick();
      }
    };
    window.addEventListener("keydown", handleShortcut);
    void Promise.all([
      desktop.settings().then((value) => {
        if (mounted) setSettings({ ...defaults, ...value });
      }),
      Promise.all([update, close]).then(() => {
        if (mounted) setConnected(true);
      }),
      drag,
      resize,
    ])
      .catch((error) => {
        if (mounted) handlers.current.notify(failure(error).message, "error");
      })
      .finally(() => {
        if (mounted) {
          void windowApi.show().catch((error) => handlers.current.notify(failure(error).message, "error"));
        }
      });
    return () => {
      mounted = false;
      window.removeEventListener("keydown", handleShortcut);
      for (const listener of [update, close, drag, resize]) {
        void listener.then((off) => off()).catch(() => {});
      }
    };
  }, []);

  useEffect(() => {
    const color = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      document.documentElement.dataset.theme =
        settings.theme === "system" ? (color.matches ? "dark" : "light") : settings.theme;
    };
    apply();
    color.addEventListener("change", apply);
    return () => color.removeEventListener("change", apply);
  }, [settings.theme]);

  function saveSettings(value: Settings) {
    setSettings(value);
    if (value.ffmpegPath !== settings.ffmpegPath) queue.recheck();
  }

  async function copy(value: string, message: string) {
    try {
      await navigator.clipboard.writeText(value);
      notify(message, "success");
    } catch {
      notify("Could not copy to the clipboard.", "error");
    }
  }

  const queued = queue.jobs.filter((item) => !isFinished(item));
  const finished = queue.jobs.filter(isFinished);
  const logTrigger = useRef<HTMLElement | null>(null);
  const logJob = queue.jobs.find(({ id }) => id === logId);

  function renderQueueRow(item: Job) {
    const hasLogs = item.status === "encoding" || queue.logs.current.has(item.id);
    const output = item.output;
    return (
      <QueueRow key={item.id} item={item}>
        {item.issue && ["outputDirectory", "permissionDenied", "diskFull"].includes(item.issue.code) && (
          <Hint label="Output settings">
            <Button
              variant="icon"
              aria-label="Output settings"
              disabled={queue.running}
              onClick={() => setSettingsOpen("output")}
            >
              <Settings2 />
            </Button>
          </Hint>
        )}
        {(item.status === "failed" || item.status === "cancelled") && (
          <Hint label={item.status === "failed" ? "Retry" : "Queue again"}>
            <Button
              variant="icon"
              aria-label={item.status === "failed" ? "Retry" : "Queue again"}
              disabled={queue.running}
              onClick={() => {
                queue.retry(item.id);
                if (item.status === "failed") void queue.start(settings, item.id);
              }}
            >
              <RotateCcw />
            </Button>
          </Hint>
        )}
        {output && (
          <>
            <Hint label="Play video">
              <Button
                variant="icon"
                aria-label="Play video"
                onClick={() => windowAction(desktop.openResult(output.path, false))}
              >
                <Play />
              </Button>
            </Hint>
            <Hint label="Show in Explorer">
              <Button
                variant="icon"
                aria-label="Show in Explorer"
                onClick={() => windowAction(desktop.openResult(output.path, true))}
              >
                <FolderOpen />
              </Button>
            </Hint>
          </>
        )}
        {hasLogs && (
          <Hint label="Logs">
            <Button
              variant="icon"
              aria-label={`Logs for ${fileLabel(item.path)}`}
              onClick={(event) => {
                logTrigger.current = event.currentTarget;
                setLogId(item.id);
              }}
            >
              <Terminal />
            </Button>
          </Hint>
        )}
        {item.status !== "encoding" && (
          <Hint label={isFinished(item) ? "Remove from finished" : "Remove from queue"}>
            <Button
              variant="icon"
              className="remove-job"
              aria-label={`Remove ${fileLabel(item.path)}`}
              onClick={() => queue.remove(item.id)}
            >
              <X />
            </Button>
          </Hint>
        )}
      </QueueRow>
    );
  }

  return (
    <Tooltip.Provider delayDuration={480} skipDelayDuration={100}>
      <div className="app">
        <header className="titlebar" data-tauri-drag-region>
          <div className="brand" data-tauri-drag-region>
            <ArrowUpRight />
            <span>upflow</span>
          </div>
          <div className="window-actions">
            <Hint label="Settings">
              <Button
                ref={settingsButton}
                variant="icon"
                className="settings-button"
                aria-label="Settings"
                disabled={queue.running || picking}
                onClick={() => setSettingsOpen("general")}
              >
                <Settings2 />
              </Button>
            </Hint>
            <div className="caption-controls">
              <button
                className="caption-button"
                aria-label="Minimize"
                onClick={() => windowAction(windowApi.minimize())}
              >
                <Minus />
              </button>
              <button
                className="caption-button"
                aria-label={maximized ? "Restore window" : "Maximize"}
                onClick={() => windowAction(windowApi.toggleMaximize())}
              >
                {maximized ? <SquaresSubtract /> : <Square />}
              </button>
              <button
                className="caption-button"
                aria-label="Close window"
                onClick={() => windowAction(windowApi.close())}
              >
                <X />
              </button>
            </div>
          </div>
        </header>

        <main className={queue.jobs.length ? "workspace" : "workspace is-empty"}>
          <div className="content">
            {queue.jobs.length === 0 ? (
              <>
                <h1 className="home-headline" aria-label={homeHeadlines[0]}>
                  {homeHeadlines.map((headline, index) => (
                    <span
                      key={headline}
                      aria-hidden="true"
                      style={{ "--phrase-offset": index } as CSSProperties}
                    >
                      {headline}
                    </span>
                  ))}
                </h1>
                <button
                  className={`drop-zone ${dragging ? "is-dragging" : ""}`}
                  disabled={picking}
                  onClick={() => void pickVideos()}
                  aria-label="Browse for videos"
                >
                  <span className="upload-icon">
                    <Upload />
                  </span>
                  <strong>{dragging ? "Release to add videos" : "Drop videos here"}</strong>
                  <span className="drop-hint">or browse files</span>
                </button>
              </>
            ) : (
              <>
                <section aria-labelledby="queued-title" className="queue-section">
                  <div className="queue-heading">
                    <h1 id="queued-title">
                      Queued <span className="queue-count">{queued.length}</span>
                    </h1>
                    <Button className="compact" onClick={() => void pickVideos()} disabled={picking}>
                      <Plus />
                      Add files
                    </Button>
                  </div>
                  {dragging && (
                    <div className="drop-target">
                      <Upload />
                      Release to add to queue
                    </div>
                  )}
                  {queued.length ? (
                    <ul className="queue-list">{queued.map(renderQueueRow)}</ul>
                  ) : (
                    !dragging && <p className="queue-empty">No files queued</p>
                  )}
                </section>
                {finished.length > 0 && (
                  <section aria-labelledby="finished-title" className="queue-section">
                    <div className="queue-heading">
                      <h2 id="finished-title">
                        Finished <span className="queue-count">{finished.length}</span>
                      </h2>
                      <Button variant="ghost" className="compact" onClick={queue.clearFinished}>
                        Clear finished
                      </Button>
                    </div>
                    <ul className="queue-list">{finished.map(renderQueueRow)}</ul>
                  </section>
                )}
              </>
            )}
          </div>
        </main>

        <footer className="footer">
          <div className="footer-content">
            <div className="inline-actions">
              {queue.running ? (
                <>
                  {activeJob && (
                    <Button
                      variant="ghost"
                      disabled={queue.stopping || queue.cancelling || activeJob.finalizing}
                      onClick={() => void queue.cancel(false)}
                    >
                      {queue.cancelling ? "Cancelling..." : "Cancel file"}
                    </Button>
                  )}
                  <Button disabled={queue.stopping} onClick={() => void queue.cancel(true)}>
                    <Square />
                    {queue.stopping ? "Stopping..." : "Stop queue"}
                  </Button>
                </>
              ) : (
                <Button
                  variant="primary"
                  disabled={!pendingCount || !connected}
                  onClick={() => void queue.start(settings)}
                >
                  {pendingCount > 1 ? `Convert ${pendingCount} files` : "Convert"}
                  <ArrowRight />
                </Button>
              )}
            </div>
          </div>
        </footer>

        <Notifications toasts={toasts} dismiss={dismiss} />
        {settingsOpen && (
          <SettingsDialog
            value={settings}
            outputFocus={settingsOpen === "output"}
            onClose={() => setSettingsOpen(null)}
            onSave={saveSettings}
            returnFocus={() => settingsButton.current?.focus()}
          />
        )}
        {logJob && (
          <LogDialog
            key={logJob.id}
            name={fileLabel(logJob.path)}
            read={() => queue.logs.current.get(logJob.id)}
            onClose={() => setLogId(null)}
            returnFocus={() => {
              if (logTrigger.current?.isConnected) logTrigger.current.focus();
              logTrigger.current = null;
            }}
            onCopy={(value) => void copy(value, "Logs copied.")}
          />
        )}
        <Dialog.Root open={closePrompt} onOpenChange={setClosePrompt}>
          <Dialog.Portal>
            <Dialog.Overlay className="modal-overlay" />
            <Dialog.Content className="modal confirmation">
              <div className="modal-header">
                <Dialog.Title>Stop converting and close?</Dialog.Title>
              </div>
              <Dialog.Description className="confirmation-copy">
                The current conversion will be cancelled. Finished videos are saved.
              </Dialog.Description>
              <div className="modal-footer">
                <Dialog.Close asChild>
                  <Button>Keep converting</Button>
                </Dialog.Close>
                <Button variant="primary" onClick={() => windowAction(desktop.confirmClose())}>
                  Stop and close
                </Button>
              </div>
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>
      </div>
    </Tooltip.Provider>
  );
}

function Hint({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Tooltip.Root>
      <Tooltip.Trigger asChild>{children}</Tooltip.Trigger>
      <Tooltip.Portal>
        <Tooltip.Content className="tooltip" side="bottom" sideOffset={7}>
          {label}
        </Tooltip.Content>
      </Tooltip.Portal>
    </Tooltip.Root>
  );
}

function QueueRow({ item, children }: { item: Job; children: ReactNode }) {
  const encoding = item.status === "encoding";
  const progress = encoding ? item.progress : undefined;
  const name = item.output?.name ?? item.media?.name ?? fileLabel(item.path);
  const metadata = item.media
    ? [`${item.media.width} × ${item.media.height}`, fpsLabel(item.media.fps), timeLabel(item.media.duration)]
    : [];
  if (item.output) metadata.push(sizeLabel(item.output.size));
  const checking = item.status === "pending" || item.status === "checking";
  const status = item.status === "pending" ? "Waiting to check file" : "Checking file";
  const phase = item.finalizing
    ? "Checking output"
    : progress
      ? `Pass ${progress.pass} of 2`
      : "Starting conversion";
  const percent = item.finalizing ? 100 : Math.floor(progress?.percent ?? 0);

  return (
    <li className={"queue-row"}>
      <div className="queue-item">
        <div className="file-summary">
          <span className="file-icon">
            <FileVideo />
          </span>
          <div className="file-copy">
            <div className="file-title">
              <strong className="truncate" title={item.output?.path ?? item.path}>
                {name}
              </strong>
              {(checking || encoding) && (
                <span className="file-status" role="status" aria-label={checking ? status : phase}>
                  <LoaderCircle className="spinner" aria-hidden="true" />
                </span>
              )}
              {(item.status === "ready" || item.status === "complete") && (
                <Hint label={item.status === "ready" ? "Ready" : "Exported"}>
                  <span
                    className="file-status"
                    tabIndex={0}
                    role="img"
                    aria-label={item.status === "ready" ? "Ready" : "Exported"}
                  >
                    <Check />
                  </span>
                </Hint>
              )}
              {item.status === "cancelled" && (
                <Hint label="Cancelled">
                  <span className="file-status" tabIndex={0} role="img" aria-label="Cancelled">
                    <CircleSlash />
                  </span>
                </Hint>
              )}
              {item.status === "failed" && (
                <Hint label="Failed">
                  <span className="file-status is-error" tabIndex={0} role="img" aria-label="Failed">
                    <CircleAlert />
                  </span>
                </Hint>
              )}
            </div>
            {metadata.length > 0 ? (
              <div className="file-meta">
                {metadata.map((value) => (
                  <span key={value}>{value}</span>
                ))}
              </div>
            ) : (
              checking && (
                <div className="metadata-skeleton" aria-hidden="true">
                  <i />
                  <i />
                  <i />
                </div>
              )
            )}
          </div>
        </div>
        <div className="row-actions">{children}</div>
      </div>
      {item.issue && <p className="row-issue">{item.issue.message}</p>}
      {encoding && (
        <div className="row-progress">
          <div className="progress-timing">
            <span>{phase}</span>
            <span className="progress-stats">
              {!item.finalizing && progress?.remaining != null && (
                <span>{remainingLabel(progress.remaining)} left</span>
              )}
              {(progress || item.finalizing) && <strong>{percent}%</strong>}
            </span>
          </div>
          <progress
            max={100}
            value={progress || item.finalizing ? percent : undefined}
            aria-label="Overall conversion progress"
          />
        </div>
      )}
    </li>
  );
}
