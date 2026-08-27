import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import type {
  EditorSnapshot,
  ExportProfile,
  HostStatus,
  JobRecord,
  ProjectAsset,
  ProjectClip,
  ProjectDocument,
  ProjectTrack,
} from "../types/editor";
import { emptyEditorSnapshot } from "../types/editor";

export type EditorCommand =
  | { type: "bundleDownload"; profile: "core" | "subtitles" | "ai" | "all" }
  | { type: "projectCreate"; name: string }
  | { type: "projectOpenRequested"; projectPath?: string }
  | { type: "projectSave" }
  | { type: "assetImportRequested" }
  | { type: "assetImport"; paths: string[] }
  | { type: "export"; outputPath: string; profile: ExportProfile; baseRevision?: number }
  | { type: "timelineApply"; baseRevision: number; operation: TimelineOperation }
  | { type: "previewPlay" }
  | { type: "previewPause" }
  | { type: "previewSeek"; timelineTicks: number }
  | { type: "jobCancel"; jobId: string }
  | { type: "subtitleGenerate"; assetId: string; language: string; baseRevision: number; trackId?: string }
  | { type: "ttsGenerate"; text: string; voiceId: string; baseRevision: number; trackId?: string }
  | { type: "assistantPlan"; baseRevision: number; text: string }
  | { type: "assistantApply"; plan: AssistantPlan };

export type TimelineOperation =
  | { AddAsset: { asset: ProjectAsset } }
  | { DeleteAsset: { asset_id: string } }
  | { AddTrack: { track: ProjectTrack } }
  | { DeleteTrack: { track_id: string } }
  | { AddClip: { track_id: string; clip: ProjectClip } }
  | { ReplaceClipAsset: { clip_id: string; asset_id: string } }
  | { RelinkAsset: { asset_id: string; relative_path: string; fingerprint: ProjectAsset["fingerprint"]; probe: ProjectAsset["probe"]; status: ProjectAsset["status"] } }
  | { SetTrackState: { track_id: string; enabled?: boolean; locked?: boolean } }
  | { SetTrackDucking: { track_id: string; ducking: { source_track_id: string; threshold_db: number; ratio: number; attack_ms: number; release_ms: number } | null } }
  | { MoveClip: { clip_id: string; timeline_start: number } }
  | { MoveClipToTrack: { clip_id: string; track_id: string; timeline_start: number } }
  | { TrimClip: { clip_id: string; source_start: number; source_end: number } }
  | { SplitClip: { clip_id: string; at_timeline_tick: number } }
  | { DeleteClip: { clip_id: string } }
  | { RippleDelete: { track_id: string; clip_id: string } }
  | { SetClipEffects: { clip_id: string; effects: ProjectClip["effects"] } }
  | { SetClipVisuals: { clip_id: string; opacity: number; transform: ProjectClip["transform"] } }
  | { AddMarker: { marker: ProjectDocument["sequence"]["markers"][number] } }
  | { DeleteMarker: { marker_id: string } };

export type EditorCommandResult =
  | { status: "accepted"; message: string; snapshot?: EditorSnapshot; subtitle?: SubtitleGenerationResponse; tts?: TtsGenerationResponse; assistant?: AssistantPlanResponse }
  | { status: "BLOCKED" | "UNAVAILABLE"; message: string };

export type EditorEvent =
  | { type: "snapshotUpdated"; snapshot: EditorSnapshot }
  | { type: "jobUpdated"; job: JobRecord }
  | { type: "connectionStatusChanged"; state: EditorSnapshot["connection"]; message: string };

export interface EditorFacade {
  getSnapshot(): Promise<EditorSnapshot>;
  chooseExportPath(profile: ExportProfile): Promise<string | null>;
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
interface ExportRequest { outputPath: string; profile: ExportProfile; baseRevision?: number }
interface BundleDownloadRequest { profile: "core" | "subtitles" | "ai" | "all" }
interface BundleDownloadResponse { profile: string; installRoot: string; mediaReady: boolean; message: string }
interface SubtitleGenerateRequest { assetId: string; language: string; baseRevision: number; trackId?: string }
interface TtsGenerateRequest { text: string; voiceId: string; baseRevision: number; trackId?: string }
export interface SubtitleGenerationResponse {
  job: JobRecord;
  document: ProjectDocument;
  language: string;
  cueCount: number | null;
  message: string;
}
export interface TtsGenerationResponse {
  job: JobRecord;
  document: ProjectDocument;
  voiceId: string;
  relativePath: string;
  message: string;
}
export interface AssistantPlan {
  base_revision: number;
  operations: Array<Record<string, unknown>>;
  warnings: string[];
  affected_clips: string[];
  requires_confirmation: boolean;
}
export interface AssistantPlanResponse {
  plan: AssistantPlan;
  provenance: { provider: string; model_id: string; model_version: string };
  message: string;
}

export const exportProfileOptions: ReadonlyArray<{ value: ExportProfile; label: string; defaultName: string }> = [
  { value: "youtube", label: "YouTube", defaultName: "youtube-export.mp4" },
  { value: "instagram", label: "Instagram", defaultName: "instagram-export.mp4" },
  { value: "tiktok", label: "TikTok", defaultName: "tiktok-export.mp4" },
];

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
  subtitle_generate: { args: SubtitleGenerateRequest; result: SubtitleGenerationResponse };
  tts_generate: { args: TtsGenerateRequest; result: TtsGenerationResponse };
  assistant_plan: { args: { baseRevision: number; text: string }; result: AssistantPlanResponse };
  assistant_apply: { args: { plan: AssistantPlan }; result: ApplyResult };
}

/** Single typed IPC boundary. Components only depend on EditorFacade. */
type CommandTransport = <K extends keyof CommandSpec>(command: K, args: CommandSpec[K]["args"]) => Promise<CommandSpec[K]["result"]>;
const tauriTransport: CommandTransport = <K extends keyof CommandSpec>(command: K, args: CommandSpec[K]["args"]) => {
  const payload = args === undefined ? undefined : { request: args };
  return invoke<CommandSpec[K]["result"]>(command, payload as Record<string, unknown> | undefined);
};
const isTauriRuntime = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const unavailableMessage = "UNAVAILABLE — Tauri host is not running. No project, media, transcript, model, or export was created.";
const projectFilters = [{ name: "VideoEditorFree project", extensions: ["vdeproj"] }];
const mediaFilters = [{ name: "Media", extensions: ["mp4", "mov", "mkv", "webm", "avi", "m4v", "ts", "mts", "m2ts", "3gp", "flv", "wmv", "mpeg", "mpg", "ogv", "wav", "mp3", "m4a", "flac", "ogg", "aac", "opus", "aiff", "aif", "mka", "ac3", "wma", "amr", "png", "jpg", "jpeg", "webp", "bmp", "srt", "vtt"] }];
const projectPathWithExtension = (path: string) => path.toLowerCase().endsWith(".vdeproj") ? path : `${path}.vdeproj`;
const exportFilters = [{ name: "MP4 video", extensions: ["mp4"] }];
const normalizePath = (path: string) => path.replaceAll("\\", "/");
const directoryOf = (path: string) => normalizePath(path).replace(/\/[^/]*$/, "") || "/";
const projectRelativePath = (selectedPath: string, projectFile: string) => {
  const root = directoryOf(projectFile).replace(/\/+$/, "");
  const normalizedSelected = normalizePath(selectedPath);
  const prefix = `${root}/`;
  if (!normalizedSelected.toLocaleLowerCase().startsWith(prefix.toLocaleLowerCase())) return null;
  const relative = normalizedSelected.slice(prefix.length);
  return relative && !relative.includes("/../") && relative !== ".." ? relative : null;
};

const statusSnapshot = (status: HostStatus, project: ProjectDocument | null = null, jobs: JobRecord[] = []): EditorSnapshot => ({
  ...emptyEditorSnapshot,
  project,
  jobs,
  capabilities: {
    mediaRuntime: status.media.state,
    assistant: { id: "local-llm", label: "Local edit assistant", state: status.ai.state, reason: status.ai.reason },
    subtitles: {
      id: "local-stt",
      label: "Local subtitle generation",
      state: status.subtitles?.state ?? "UNAVAILABLE",
      reason: status.subtitles?.reason ?? "The host does not expose a verified subtitle generation capability.",
    },
    tts: {
      id: "local-tts",
      label: "Local voiceover generation",
      state: status.tts?.state ?? "UNAVAILABLE",
      reason: status.tts?.reason ?? "The host does not expose a verified Piper voice runtime.",
    },
    effects: status.effects ?? {
      state: "UNAVAILABLE",
      reason: "The host does not expose typed visual effects.",
    },
    audioDucking: status.audioDucking ?? {
      state: "UNAVAILABLE",
      reason: "The host does not expose typed audio ducking.",
    },
    exportProfiles: status.exportProfiles ?? {
      state: "UNAVAILABLE",
      reason: "The host does not expose platform export profiles.",
    },
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
  subtitleGenerate: "subtitle_generate",
  ttsGenerate: "tts_generate",
  assistantPlan: "assistant_plan",
  assistantApply: "assistant_apply",
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
const isUnavailableHostError = (error: unknown) => /(?:UNAVAILABLE|NOT_PROVISIONED|MEDIA_UNAVAILABLE|PREVIEW_UNAVAILABLE|AI_UNAVAILABLE|AI_BUSY|AI_PLAN_TIMEOUT|AI_PLAN_OUTPUT_LIMIT|AI_PLAN_FAILED|AI_PLAN_INVALID|TTS_RUNTIME_UNAVAILABLE|TTS_GENERATION_FAILED|TTS_GENERATION_CANCELLED|SUBTITLE_RUNTIME_UNAVAILABLE|AI_TRANSCRIPTION_FAILED|AI_TRANSCRIPTION_OUTPUT_LIMIT|AI_TRANSCRIPTION_CANCELLED|BUNDLE_DOWNLOAD_FAILED|BUNDLE_SCRIPT_MISSING|BUNDLE_MANIFEST_MISSING)/i.test(transportErrorCode(error))
  || /(?:not provisioned|no (?:reviewed|verified) .* provisioned|runtime is unavailable)/i.test(transportErrorMessage(error));

const createTauriEditorFacade = (transport: CommandTransport): EditorFacade => {
  let snapshot = emptyEditorSnapshot;
  let projectPath: string | null = null;
  const listeners = new Set<(event: EditorEvent) => void>();
  const notify = (event: EditorEvent) => listeners.forEach((listener) => listener(event));
  const chooseProjectPath = async (title: string) => {
    const selected = await save({ title, defaultPath: "Untitled.vdeproj", filters: projectFilters });
    return typeof selected === "string" && selected.trim() ? projectPathWithExtension(selected.trim()) : null;
  };
  const setProject = (project: ProjectDocument, message: string, path?: string) => {
    if (path) projectPath = path;
    snapshot = { ...snapshot, project, connection: "READY", connectionMessage: message };
    notify({ type: "snapshotUpdated", snapshot });
  };

  return {
    chooseExportPath: async (profile) => {
      if (!projectPath) return null;
      const option = exportProfileOptions.find((item) => item.value === profile) ?? exportProfileOptions[0];
      const selected = await save({
        title: `Export ${option.label} video`,
        defaultPath: `${directoryOf(projectPath)}/${option.defaultName}`,
        filters: exportFilters,
      });
      if (typeof selected !== "string" || !selected.trim()) return null;
      return projectRelativePath(selected.trim(), projectPath);
    },
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
            const selectedPath = await chooseProjectPath("Create VideoEditorFree project");
            if (!selectedPath) return { status: "accepted", message: "Project creation cancelled." };
            const project = await transport("project_create", { name: command.name, projectPath: selectedPath });
            setProject(project, `Project created: ${project.name}.`, selectedPath);
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
            setProject(project, `Project opened: ${project.name}.`, selectedPath);
            return { status: "accepted", message: snapshot.connectionMessage, snapshot };
          }
          case "projectSave": {
            const selectedPath = projectPath ?? await chooseProjectPath("Save VideoEditorFree project");
            if (!selectedPath) return { status: "accepted", message: "Save cancelled." };
            const result = await transport("project_save", { projectPath: selectedPath });
            projectPath = selectedPath;
            const message = `Project saved: ${result.bytesWritten} bytes${result.backupCreated ? "; backup created." : "."}`;
            snapshot = { ...snapshot, connectionMessage: message };
            notify({ type: "snapshotUpdated", snapshot });
            return { status: "accepted", message, snapshot };
          }
          case "assetImportRequested": {
            if (snapshot.capabilities.mediaRuntime !== "READY") {
              return { status: "UNAVAILABLE", message: "UNAVAILABLE — download and verify the Core bundle before importing media." };
            }
            const selected = await open({
              title: "Import media",
              multiple: true,
              directory: false,
              filters: mediaFilters,
            });
            const paths = selected === null ? [] : Array.isArray(selected) ? selected : [selected];
            if (paths.length === 0) {
              return { status: "accepted", message: "No media selected." };
            }
            if (!snapshot.project) {
              const selectedPath = await chooseProjectPath("Choose project location before importing media");
              if (!selectedPath) return { status: "accepted", message: "Import cancelled; no project was created." };
              const project = await transport("project_create", { name: "Untitled project", projectPath: selectedPath });
              setProject(project, `Project created: ${project.name}.`, selectedPath);
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
            if (snapshot.capabilities.exportProfiles.state !== "READY") {
              return { status: "UNAVAILABLE", message: `UNAVAILABLE — ${snapshot.capabilities.exportProfiles.reason}` };
            }
            const job = await transport("export_start", { outputPath: command.outputPath, profile: command.profile, baseRevision: command.baseRevision });
            snapshot = { ...snapshot, jobs: [...snapshot.jobs.filter((item) => item.id !== job.id), job] };
            notify({ type: "jobUpdated", job });
            return { status: "accepted", message: "Export job started.", snapshot };
          }
          case "timelineApply": {
            if ("SetTrackDucking" in command.operation && snapshot.capabilities.audioDucking.state !== "READY") {
              return { status: "UNAVAILABLE", message: `UNAVAILABLE — ${snapshot.capabilities.audioDucking.reason}` };
            }
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
          case "subtitleGenerate": {
            if (snapshot.capabilities.subtitles.state !== "READY") {
              return { status: "UNAVAILABLE", message: `UNAVAILABLE — ${snapshot.capabilities.subtitles.reason}` };
            }
            const result = await transport("subtitle_generate", {
              assetId: command.assetId,
              language: command.language,
              baseRevision: command.baseRevision,
              trackId: command.trackId,
            });
            setProject(result.document, `Subtitles generated at revision ${result.document.revision}.`);
            snapshot = { ...snapshot, jobs: [...snapshot.jobs.filter((item) => item.id !== result.job.id), result.job] };
            notify({ type: "jobUpdated", job: result.job });
            return { status: "accepted", message: result.message, snapshot, subtitle: result };
          }
          case "ttsGenerate": {
            if (snapshot.capabilities.tts.state !== "READY") {
              return { status: "UNAVAILABLE", message: `UNAVAILABLE — ${snapshot.capabilities.tts.reason}` };
            }
            const result = await transport("tts_generate", {
              text: command.text,
              voiceId: command.voiceId,
              baseRevision: command.baseRevision,
              trackId: command.trackId,
            });
            setProject(result.document, `Voiceover generated at revision ${result.document.revision}.`);
            snapshot = { ...snapshot, jobs: [...snapshot.jobs.filter((item) => item.id !== result.job.id), result.job] };
            notify({ type: "jobUpdated", job: result.job });
            return { status: "accepted", message: result.message, snapshot, tts: result };
          }
          case "assistantPlan": {
            if (snapshot.capabilities.assistant.state !== "READY") {
              return { status: "UNAVAILABLE", message: `UNAVAILABLE — ${snapshot.capabilities.assistant.reason}` };
            }
            const result = await transport("assistant_plan", { baseRevision: command.baseRevision, text: command.text });
            return { status: "accepted", message: result.message, assistant: result };
          }
          case "assistantApply": {
            const result = await transport("assistant_apply", { plan: command.plan });
            setProject(result.document, `Assistant plan applied at revision ${result.document.revision}.`);
            return { status: "accepted", message: snapshot.connectionMessage, snapshot };
          }
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
  chooseExportPath: async () => null,
  dispatch: async () => ({ status: "UNAVAILABLE", message: unavailableMessage }),
  subscribe: () => () => undefined,
});

export const editorFacade: EditorFacade = isTauriRuntime() ? createTauriEditorFacade(tauriTransport) : createUnavailableEditorFacade();
