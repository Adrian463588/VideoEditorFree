import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { EditorSnapshot, HostStatus, JobRecord, ProjectDocument } from "../types/editor";
import { emptyEditorSnapshot } from "../types/editor";

export type EditorCommand =
  | { type: "bundleDownload"; profile: "core" | "ai" | "all" }
  | { type: "projectCreate"; name: string }
  | { type: "projectOpenRequested"; projectPath?: string }
  | { type: "projectSave" }
  | { type: "assetImportRequested" }
  | { type: "assetImport"; paths: string[] }
  | { type: "export"; outputPath: string; baseRevision?: number }
  | { type: "timelineApply"; baseRevision: number; operation: TimelineOperation }
  | { type: "previewPlay" }
  | { type: "previewPause" }
  | { type: "previewSeek"; timelineTicks: number }
  | { type: "jobCancel"; jobId: string }
  | { type: "assistantPlan"; baseRevision: number; text: string };

export type TimelineOperation =
  | { SplitClip: { clip_id: string; at_timeline_tick: number } }
  | { DeleteClip: { clip_id: string } };

export type EditorCommandResult =
  | { status: "accepted"; message: string; snapshot?: EditorSnapshot }
  | { status: "BLOCKED" | "UNAVAILABLE"; message: string };

export type EditorEvent =
  | { type: "snapshotUpdated"; snapshot: EditorSnapshot }
  | { type: "jobUpdated"; job: JobRecord }
  | { type: "connectionStatusChanged"; state: EditorSnapshot["connection"]; message: string };

export interface EditorFacade {
  getSnapshot(): Promise<EditorSnapshot>;
  dispatch(command: EditorCommand): Promise<EditorCommandResult>;
  subscribe(listener: (event: EditorEvent) => void): () => void;
}

interface ProjectCreateRequest { name: string; projectId?: string; projectPath?: string }
interface ProjectOpenRequest { projectPath: string }
interface ProjectSaveRequest { projectPath?: string; expectedRevision?: number }
interface SaveProjectResponse { bytesWritten: number; backupCreated: boolean }
interface TimelineApplyRequest { baseRevision: number; operation: TimelineOperation }
interface ApplyResult { document: ProjectDocument; undo: { previous: ProjectDocument; applied_revision: number } }
interface JobRequest { jobId: string }
interface ExportRequest { outputPath: string; baseRevision?: number }
interface BundleDownloadRequest { profile: "core" | "ai" | "all" }
interface BundleDownloadResponse { profile: string; installRoot: string; mediaReady: boolean; message: string }

interface CommandSpec {
  bundle_download: { args: BundleDownloadRequest; result: BundleDownloadResponse };
  host_status: { args: undefined; result: HostStatus };
  project_create: { args: ProjectCreateRequest; result: ProjectDocument };
  project_open: { args: ProjectOpenRequest; result: ProjectDocument };
  project_save: { args: ProjectSaveRequest; result: SaveProjectResponse };
  timeline_apply: { args: TimelineApplyRequest; result: ApplyResult };
  preview_play: { args: undefined; result: void };
  preview_pause: { args: undefined; result: void };
  preview_seek: { args: { timelineTicks: number }; result: void };
  asset_import: { args: { paths: string[] }; result: ProjectDocument };
  job_cancel: { args: JobRequest; result: JobRecord };
  job_get: { args: JobRequest; result: JobRecord };
  job_list: { args: undefined; result: JobRecord[] };
  export: { args: ExportRequest; result: JobRecord };
  export_start: { args: ExportRequest; result: JobRecord };
  assistant_plan: { args: { baseRevision: number; text: string }; result: void };
}

/** Single typed IPC boundary. Components only depend on EditorFacade. */
type CommandTransport = <K extends keyof CommandSpec>(command: K, args: CommandSpec[K]["args"]) => Promise<CommandSpec[K]["result"]>;
const tauriTransport: CommandTransport = <K extends keyof CommandSpec>(command: K, args: CommandSpec[K]["args"]) => invoke<CommandSpec[K]["result"]>(command, args as Record<string, unknown> | undefined);
const isTauriRuntime = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const unavailableMessage = "UNAVAILABLE — Tauri host is not running. No project, media, transcript, model, or export was created.";

const statusSnapshot = (status: HostStatus, project: ProjectDocument | null = null, jobs: JobRecord[] = []): EditorSnapshot => ({
  ...emptyEditorSnapshot,
  project,
  jobs,
  capabilities: {
    mediaRuntime: status.media.state,
    assistant: { id: "local-llm", label: "Local edit assistant", state: status.ai.state, reason: status.ai.reason },
  },
  connection: status.core.state,
  connectionMessage: status.core.reason,
});

const commandName = (command: EditorCommand) => ({
  bundleDownload: "bundle_download",
  projectCreate: "project_create",
  projectOpenRequested: "project_open",
  projectSave: "project_save",
  assetImportRequested: "asset_import",
  assetImport: "asset_import",
  export: "export_start",
  timelineApply: "timeline_apply",
  previewPlay: "preview_play",
  previewPause: "preview_pause",
  previewSeek: "preview_seek",
  jobCancel: "job_cancel",
  assistantPlan: "assistant_plan",
}[command.type]);

const transportErrorMessage = (error: unknown) => {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  if (error && typeof error === "object" && "message" in error) return String(error.message);
  return String(error);
};

const transportErrorCode = (error: unknown) => {
  if (error && typeof error === "object" && "code" in error) return String(error.code);
  return "";
};

const isMissingHostCommand = (error: unknown) => /(?:command|invoke).*(?:not found|unknown|unavailable|not allowed)/i.test(transportErrorMessage(error));
  const isUnavailableHostError = (error: unknown) => /(?:UNAVAILABLE|NOT_PROVISIONED|BUNDLE_DOWNLOAD_FAILED|BUNDLE_SCRIPT_MISSING|BUNDLE_MANIFEST_MISSING)/i.test(transportErrorCode(error))
  || /(?:not provisioned|no reviewed .* provisioned|runtime is unavailable)/i.test(transportErrorMessage(error));

const createTauriEditorFacade = (transport: CommandTransport): EditorFacade => {
  let snapshot = emptyEditorSnapshot;
  const listeners = new Set<(event: EditorEvent) => void>();
  const notify = (event: EditorEvent) => listeners.forEach((listener) => listener(event));
  const setProject = (project: ProjectDocument, message: string) => {
    snapshot = { ...snapshot, project, connection: "READY", connectionMessage: message };
    notify({ type: "snapshotUpdated", snapshot });
  };

  return {
    getSnapshot: async () => {
      const hostStatus = await transport("host_status", undefined);
      const jobs = await transport("job_list", undefined);
      snapshot = statusSnapshot(hostStatus, snapshot.project, jobs);
      notify({ type: "snapshotUpdated", snapshot });
      return snapshot;
    },
    dispatch: async (command) => {
      try {
        switch (command.type) {
          case "bundleDownload": {
            const result = await transport("bundle_download", { profile: command.profile });
            const hostStatus = await transport("host_status", undefined);
            snapshot = statusSnapshot(hostStatus, snapshot.project, snapshot.jobs);
            snapshot = { ...snapshot, connectionMessage: result.message };
            notify({ type: "snapshotUpdated", snapshot });
            return { status: "accepted", message: `${result.message} Install root: ${result.installRoot}`, snapshot };
          }
          case "projectCreate": {
            const project = await transport("project_create", { name: command.name });
            setProject(project, `Project created: ${project.name}.`);
            return { status: "accepted", message: snapshot.connectionMessage, snapshot };
          }
          case "projectOpenRequested": {
            const selectedPath = command.projectPath ?? await open({
              title: "Open VideoEditorFree project",
              multiple: false,
              directory: false,
              filters: [{ name: "VideoEditorFree project", extensions: ["vdeproj"] }],
            });
            if (typeof selectedPath !== "string" || !selectedPath.trim()) {
              return { status: "accepted", message: "No project selected." };
            }
            const project = await transport("project_open", { projectPath: selectedPath });
            setProject(project, `Project opened: ${project.name}.`);
            return { status: "accepted", message: snapshot.connectionMessage, snapshot };
          }
          case "projectSave": {
            const result = await transport("project_save", {});
            const message = `Project saved: ${result.bytesWritten} bytes${result.backupCreated ? "; backup created." : "."}`;
            snapshot = { ...snapshot, connectionMessage: message };
            notify({ type: "snapshotUpdated", snapshot });
            return { status: "accepted", message, snapshot };
          }
          case "assetImportRequested": {
            const selected = await open({
              title: "Import media",
              multiple: true,
              directory: false,
              filters: [{ name: "Media", extensions: ["mp4", "mov", "mkv", "webm", "avi", "m4v", "wav", "mp3", "m4a", "flac", "ogg", "png", "jpg", "jpeg", "webp", "bmp", "srt", "vtt"] }],
            });
            const paths = selected === null ? [] : Array.isArray(selected) ? selected : [selected];
            if (paths.length === 0) {
              return { status: "accepted", message: "No media selected." };
            }
            const project = await transport("asset_import", { paths });
            setProject(project, "Assets imported.");
            return { status: "accepted", message: "Assets imported.", snapshot };
          }
          case "assetImport":
            {
              const project = await transport("asset_import", { paths: command.paths });
              setProject(project, "Assets imported.");
              return { status: "accepted", message: "Assets imported.", snapshot };
            }
          case "export": {
            const job = await transport("export_start", { outputPath: command.outputPath, baseRevision: command.baseRevision });
            snapshot = { ...snapshot, jobs: [...snapshot.jobs.filter((item) => item.id !== job.id), job] };
            notify({ type: "jobUpdated", job });
            return { status: "accepted", message: "Export job started.", snapshot };
          }
          case "timelineApply": {
            const result = await transport("timeline_apply", { baseRevision: command.baseRevision, operation: command.operation });
            setProject(result.document, `Timeline updated at revision ${result.document.revision}.`);
            return { status: "accepted", message: snapshot.connectionMessage, snapshot };
          }
          case "previewPlay":
            if (snapshot.capabilities.mediaRuntime !== "READY") {
              return { status: "UNAVAILABLE", message: "UNAVAILABLE — preview media runtime is not provisioned; playback did not start." };
            }
            await transport("preview_play", undefined);
            return { status: "accepted", message: "Preview playing." };
          case "previewPause":
            if (snapshot.capabilities.mediaRuntime !== "READY") {
              return { status: "UNAVAILABLE", message: "UNAVAILABLE — preview media runtime is not provisioned; playback did not pause." };
            }
            await transport("preview_pause", undefined);
            return { status: "accepted", message: "Preview paused." };
          case "previewSeek":
            if (snapshot.capabilities.mediaRuntime !== "READY") {
              return { status: "UNAVAILABLE", message: "UNAVAILABLE — preview media runtime is not provisioned; playhead did not move." };
            }
            await transport("preview_seek", { timelineTicks: command.timelineTicks });
            return { status: "accepted", message: "Preview position updated." };
          case "jobCancel":
            {
              const job = await transport("job_cancel", { jobId: command.jobId });
              snapshot = { ...snapshot, jobs: snapshot.jobs.map((item) => item.id === job.id ? job : item) };
              notify({ type: "jobUpdated", job });
            }
            return { status: "accepted", message: `Cancellation requested for job ${command.jobId}.` };
         case "assistantPlan":
           await transport("assistant_plan", { baseRevision: command.baseRevision, text: command.text });
           return { status: "accepted", message: "Assistant plan ready for review." };
       }
      } catch (error) {
        if (isUnavailableHostError(error)) {
          return { status: "UNAVAILABLE", message: "UNAVAILABLE — " + transportErrorMessage(error) };
        }
        if (isMissingHostCommand(error)) {
          return { status: "BLOCKED", message: `BLOCKED — host command "${commandName(command)}" is not exposed; no change was applied.` };
        }
        throw error;
      }
    },
    subscribe: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
};

export const createUnavailableEditorFacade = (): EditorFacade => ({
  getSnapshot: async () => ({ ...emptyEditorSnapshot, connection: "UNAVAILABLE", connectionMessage: unavailableMessage }),
  dispatch: async () => ({ status: "UNAVAILABLE", message: unavailableMessage }),
  subscribe: () => () => undefined,
});

export const editorFacade: EditorFacade = isTauriRuntime() ? createTauriEditorFacade(tauriTransport) : createUnavailableEditorFacade();
