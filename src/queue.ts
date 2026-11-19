import { useRef, useState } from "react";
import {
  desktop,
  failure,
  type Failure,
  type Media,
  type Output,
  type Progress,
  type Settings,
  type Update,
} from "./native";

type JobStatus = "pending" | "checking" | "ready" | "encoding" | "complete" | "failed" | "cancelled";

export type Job = {
  id: string;
  path: string;
  status: JobStatus;
  media?: Media;
  output?: Output;
  issue?: Failure;
  progress?: Progress;
  finalizing?: boolean;
};

export function isFinished({ status }: Job) {
  return status === "complete" || status === "failed" || status === "cancelled";
}

export type Notify = (
  message: string,
  kind?: "info" | "success" | "error",
  action?: { label: string; run: () => void },
) => void;

export type JobLog = { lines: string[]; firstLine: number };

const maxLogLines = 500;

export function useQueue(notify: Notify, openLogs: (id: string) => void) {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  // Native callbacks and the conversion loop read the latest queue between renders.
  const jobsRef = useRef<Job[]>([]);
  const active = useRef<{ id: string; attempt: string } | null>(null);
  const runningRef = useRef(false);
  const stopRequested = useRef(false);
  const inspectionTask = useRef<Promise<void> | null>(null);
  const nextId = useRef(0);
  const logs = useRef(new Map<string, JobLog>());

  function replaceJobs(update: (previous: Job[]) => Job[]) {
    jobsRef.current = update(jobsRef.current);
    setJobs(jobsRef.current);
  }

  function patch(id: string, changes: Partial<Job>) {
    replaceJobs((previous) => previous.map((job) => (job.id === id ? { ...job, ...changes } : job)));
  }

  function appendLog(id: string, line: string) {
    const log = logs.current.get(id) ?? { lines: [], firstLine: 1 };
    log.lines.push(...line.split(/\r\n|\r|\n/));
    const overflow = log.lines.length - maxLogLines;
    if (overflow > 0) {
      log.lines.splice(0, overflow);
      log.firstLine += overflow;
    }
    logs.current.set(id, log);
  }

  function markFailed(job: Job, issue: Failure) {
    appendLog(job.id, [issue.message, issue.detail].filter(Boolean).join("\n"));
    patch(job.id, { status: "failed", issue });
    notify(issue.message, "error", {
      label: "Logs",
      run: () => {
        if (jobsRef.current.some(({ id }) => id === job.id)) openLogs(job.id);
      },
    });
  }

  async function inspect(job: Job) {
    patch(job.id, { status: "checking", issue: undefined });
    try {
      const media = await desktop.inspect(job.path);
      patch(job.id, { media, status: "ready" });
    } catch (error) {
      if (jobsRef.current.some((candidate) => candidate.id === job.id)) markFailed(job, failure(error));
    }
  }

  function inspectPending() {
    if (runningRef.current || inspectionTask.current) return;
    inspectionTask.current = (async () => {
      while (!runningRef.current) {
        const next = jobsRef.current.find((job) => job.status === "pending");
        if (!next) break;
        await inspect(next);
      }
    })().finally(() => {
      inspectionTask.current = null;
      if (jobsRef.current.some((job) => job.status === "pending")) inspectPending();
    });
  }

  function add(paths: string[]) {
    const queuedPaths = new Set(
      jobsRef.current.filter((job) => job.status !== "complete").map((job) => job.path.toLowerCase()),
    );
    const newJobs: Job[] = [];
    for (const path of paths) {
      const key = path.toLowerCase();
      if (queuedPaths.has(key)) continue;
      queuedPaths.add(key);
      newJobs.push({ id: String(++nextId.current), path, status: "pending" });
    }
    if (!newJobs.length) {
      if (paths.length) notify("Already in the queue.");
      return;
    }
    replaceJobs((previous) => [...previous, ...newJobs]);
    inspectPending();
  }

  function remove(id: string) {
    if (active.current?.id === id) return;
    replaceJobs((previous) => previous.filter((job) => job.id !== id));
    logs.current.delete(id);
  }

  function clearFinished() {
    replaceJobs((previous) =>
      previous.filter((job) => {
        if (!isFinished(job)) return true;
        logs.current.delete(job.id);
        return false;
      }),
    );
  }

  function retry(id: string) {
    const job = jobsRef.current.find((job) => job.id === id);
    if (!job) return;
    logs.current.delete(id);
    patch(id, {
      issue: undefined,
      progress: undefined,
      finalizing: false,
      status: job.media ? "ready" : "pending",
    });
    inspectPending();
  }

  function recheck() {
    replaceJobs((previous) =>
      previous.map((job) => {
        if (job.status === "complete") return job;
        const id = String(++nextId.current);
        logs.current.delete(job.id);
        return { id, path: job.path, status: "pending" };
      }),
    );
    inspectPending();
  }

  async function start(settings: Settings, onlyJobId?: string) {
    const matchesRequestedJob = (job: Job) => !onlyJobId || job.id === onlyJobId;
    if (
      runningRef.current ||
      !jobsRef.current.some(
        (job) => matchesRequestedJob(job) && ["ready", "pending", "checking"].includes(job.status),
      )
    ) {
      return;
    }
    runningRef.current = true;
    stopRequested.current = false;
    setRunning(true);
    let completed = 0;
    let lastOutput: Output | undefined;

    try {
      await inspectionTask.current;
      while (!stopRequested.current) {
        const job = jobsRef.current.find(
          (candidate) =>
            matchesRequestedJob(candidate) &&
            (candidate.status === "ready" || candidate.status === "pending"),
        );
        if (!job) break;
        if (job.status === "pending") {
          await inspect(job);
          continue;
        }

        const attempt = String(++nextId.current);
        active.current = { id: job.id, attempt };
        logs.current.delete(job.id);
        patch(job.id, { status: "encoding", issue: undefined, finalizing: false, progress: undefined });
        try {
          const output = await desktop.convert(attempt, job.path, null);
          patch(job.id, { output, status: "complete" });
          lastOutput = output;
          completed++;
        } catch (error) {
          const issue = failure(error);
          if (issue.code === "cancelled") {
            patch(job.id, { status: "cancelled" });
            if (!stopRequested.current) notify("Conversion cancelled.");
          } else {
            markFailed(job, issue);
          }
        } finally {
          active.current = null;
          setCancelling(false);
        }
      }
    } finally {
      if (stopRequested.current) notify("Queue stopped.");
      runningRef.current = false;
      stopRequested.current = false;
      setRunning(false);
      setStopping(false);
      inspectPending();
    }

    if (completed) notify(completed === 1 ? "Video exported." : `${completed} videos exported.`, "success");
    if (lastOutput && settings.openFolderAfterConversion) {
      try {
        await desktop.openFolder(lastOutput.directory);
      } catch (error) {
        notify(failure(error).message, "error");
      }
    }
  }

  async function cancel(stopAll: boolean) {
    if (stopAll && !stopRequested.current) {
      stopRequested.current = true;
      setStopping(true);
    }
    if (active.current) {
      setCancelling(true);
      try {
        await desktop.cancel(active.current.attempt);
      } catch (error) {
        setCancelling(false);
        notify(failure(error).message, "error");
      }
    }
  }

  function onUpdate(update: Update) {
    const current = active.current;
    if (!current || update.jobId !== current.attempt) return;
    if (update.type === "log") appendLog(current.id, update.line);
    if (update.type === "progress") patch(current.id, { progress: update });
    if (update.type === "finalizing") patch(current.id, { finalizing: true });
  }

  return {
    jobs,
    running,
    runningRef,
    stopping,
    cancelling,
    logs,
    add,
    remove,
    clearFinished,
    retry,
    recheck,
    start,
    cancel,
    onUpdate,
  };
}
