import { useEffect, useMemo, useRef, useState, type DragEvent, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import { editorFacade, exportProfileOptions, type AssistantPlanResponse, type EditorCommand, type EditorCommandResult } from "./api/editorFacade";
import { Icon, type IconName } from "./components/Icon";
import { uiProjection, useUiProjection } from "./stores/uiProjection";
import { emptyEditorSnapshot, type Effect, type EditorSnapshot, type ExportProfile, type ProjectAsset, type ProjectClip, type ProjectTrack, type Rational } from "./types/editor";

const formatTicks = (ticks: number, timebase: Rational | null) => {
  if (!timebase || timebase.numerator <= 0 || timebase.denominator <= 0) return "—:—";
  const seconds = Math.max(0, Math.floor((ticks * timebase.denominator) / timebase.numerator));
  return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
};
const assetName = (asset: ProjectAsset) => asset.relative_path.split(/[\\/]/).pop() ?? asset.relative_path;
const effectName = (effect: Effect) => Object.keys(effect)[0] ?? "Unknown";
const errorDetail = (error: unknown) => error instanceof Error ? error.message : typeof error === "string" ? error : "Unknown host error.";
const mediaAssetKinds = new Set<ProjectAsset["kind"]>(["Video", "Audio", "Image"]);
const subtitleLanguages = [
  ["auto", "Auto detect"], ["en", "English"], ["id", "Bahasa Indonesia"], ["es", "Español"],
  ["fr", "Français"], ["de", "Deutsch"], ["it", "Italiano"], ["pt", "Português"],
  ["ja", "日本語"], ["ko", "한국어"], ["zh", "中文"], ["ru", "Русский"], ["ar", "العربية"],
  ["hi", "हिन्दी"], ["nl", "Nederlands"], ["tr", "Türkçe"], ["pl", "Polski"],
  ["uk", "Українська"], ["vi", "Tiếng Việt"],
] as const;
const createId = (prefix: string) => `${prefix}-${typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : Date.now().toString(36)}`;
const durationSecondsToTicks = (seconds: number, timebase: Rational) => Math.max(1, Math.round((seconds * timebase.numerator) / timebase.denominator));
const clipEnd = (clip: ProjectClip) => clip.timeline_start + clip.timeline_duration;
const trackKindForAsset = (asset: ProjectAsset, forced?: "Video" | "Audio" | "Overlay") => {
  if (forced) return forced;
  return asset.kind === "Audio" ? "Audio" as const : asset.kind === "Subtitle" ? "Subtitle" as const : "Video" as const;
};

const effectPresets: ReadonlyArray<{ label: string; value: Effect }> = [
  { label: "Brightness +10%", value: { Brightness: { value: 0.1 } } },
  { label: "Contrast +20%", value: { Contrast: { value: 1.2 } } },
  { label: "Saturation +20%", value: { Saturation: { value: 1.2 } } },
  { label: "Exposure +1", value: { Exposure: { value: 1 } } },
  { label: "Gamma 1.15", value: { Gamma: { value: 1.15 } } },
  { label: "Warm 4500K", value: { Temperature: { kelvin: 4500 } } },
  { label: "Tint +10%", value: { Tint: { value: 0.1 } } },
  { label: "Three-way color balance", value: { ColorBalance: { shadows: { red: 0.05, green: 0, blue: -0.05 }, midtones: { red: 0.08, green: 0.02, blue: -0.08 }, highlights: { red: 0.1, green: 0.03, blue: -0.1 } } } },
  { label: "Blur", value: { Blur: { radius: 4 } } },
  { label: "Sharpen", value: { Sharpen: { amount: 1 } } },
  { label: "Vignette", value: { Vignette: { amount: 0.5 } } },
  { label: "Duotone", value: { Duotone: { shadows: { red: 12, green: 20, blue: 55 }, highlights: { red: 255, green: 190, blue: 120 } } } },
  { label: "3D LUT (.cube)", value: { Lut: { relative_path: "looks/custom.cube" } } },
  { label: "Audio +3 dB", value: { Volume: { gain_db: 3 } } },
  { label: "Audio fade in", value: { Fade: { kind: "In", duration_ticks: 30 } } },
  { label: "Audio fade out", value: { Fade: { kind: "Out", duration_ticks: 30 } } },
];

function SectionHeading({ icon, eyebrow, title, action }: { icon: IconName; eyebrow: string; title: string; action?: ReactNode }) {
  return <div className="section-heading"><div className="section-heading-copy"><span className="eyebrow"><Icon name={icon} size={13} /> {eyebrow}</span><h2>{title}</h2></div>{action}</div>;
}
function StatusPill({ state, children }: { state: "READY" | "BLOCKED" | "UNAVAILABLE"; children: ReactNode }) {
  return <span className={`status-pill status-${state.toLowerCase()}`}><span className="status-dot" />{children}</span>;
}
function EmptyState({ icon, title, children, action }: { icon: IconName; title: string; children: ReactNode; action?: ReactNode }) {
  return <div className="empty-state"><div className="empty-icon"><Icon name={icon} size={22} /></div><strong>{title}</strong><p>{children}</p>{action}</div>;
}

export function App() {
  const [snapshot, setSnapshot] = useState<EditorSnapshot>(emptyEditorSnapshot);
  const projection = useUiProjection();
  const [commandStatus, setCommandStatus] = useState(emptyEditorSnapshot.connectionMessage);
  const [liveError, setLiveError] = useState<string | null>(null);
  const [bundleBusy, setBundleBusy] = useState(false);
  const [bundleProfile, setBundleProfile] = useState<"core" | "subtitles" | "ai" | "all">("core");
  const [exportProfile, setExportProfile] = useState<ExportProfile>("youtube");

  const reportError = (operation: string, error: unknown) => {
    const message = `${operation} failed: ${errorDetail(error)}`;
    setCommandStatus(message);
    setLiveError(message);
    uiProjection.patch({ announcement: message });
  };
  useEffect(() => {
    const unsubscribe = editorFacade.subscribe((event) => {
      if (event.type === "snapshotUpdated") {
        setSnapshot(event.snapshot);
        setCommandStatus(event.snapshot.connectionMessage);
      } else if (event.type === "connectionStatusChanged") {
        setCommandStatus(event.message);
        uiProjection.patch({ announcement: event.message });
      }
    });
    let active = true;
    let refreshInFlight = false;
    const loadSnapshot = async () => {
      if (!active || refreshInFlight) return;
      refreshInFlight = true;
      try {
        const next = await editorFacade.getSnapshot();
        if (active) {
          setSnapshot(next);
          setCommandStatus(next.connectionMessage);
        }
      } catch (error) {
        if (active) reportError("Loading editor host status", error);
      } finally {
        refreshInFlight = false;
      }
    };
    void loadSnapshot();
    const refreshTimer = window.setInterval(() => { void loadSnapshot(); }, 1000);
    return () => { active = false; window.clearInterval(refreshTimer); unsubscribe(); };
  }, []);

  const dispatch = async (command: EditorCommand): Promise<EditorCommandResult> => {
    try {
      const result = await editorFacade.dispatch(command);
      setCommandStatus(result.message);
      setLiveError(result.status === "accepted" ? null : result.message);
      uiProjection.patch({ announcement: result.message });
      if (result.status === "accepted" && result.snapshot) setSnapshot(result.snapshot);
      if (result.status === "accepted" && (command.type === "projectCreate" || command.type === "projectOpenRequested")) {
        uiProjection.patch({ selectedAssetId: null, selectedClipId: null, playheadTicks: 0, isPlaying: false });
      }
      if (result.status === "accepted" && command.type === "timelineApply" && ("DeleteClip" in command.operation || "RippleDelete" in command.operation)) {
        uiProjection.patch({ selectedAssetId: null, selectedClipId: null });
      }
      return result;
    } catch (error) {
      reportError(`Command ${command.type}`, error);
      return { status: "BLOCKED", message: `${command.type} failed: ${errorDetail(error)}` };
    }
  };

  const selectedAsset = useMemo(() => snapshot.project?.assets.find((asset) => asset.id === projection.selectedAssetId) ?? null, [projection.selectedAssetId, snapshot.project]);
  const tracks = snapshot.project?.sequence.tracks ?? [];
  const selectedClip = useMemo(() => tracks.flatMap((track) => track.clips).find((clip) => clip.id === projection.selectedClipId) ?? null, [projection.selectedClipId, tracks]);

  const exportProject = async () => {
    if (!snapshot.project) {
      reportError("Export", "Create or open a project before exporting.");
      return;
    }
    const outputPath = await editorFacade.chooseExportPath(exportProfile);
    if (!outputPath) {
      const message = "Export cancelled or the destination must be inside the project folder.";
      setCommandStatus(message);
      uiProjection.patch({ announcement: message });
      return;
    }
    void dispatch({ type: "export", outputPath, profile: exportProfile, baseRevision: snapshot.project.revision });
  };
  const downloadBundle = async () => {
    setBundleBusy(true);
    try { await dispatch({ type: "bundleDownload", profile: bundleProfile }); } finally { setBundleBusy(false); }
  };

  const addSelectedAssetToTimeline = async (forcedKind?: "Video" | "Audio" | "Overlay") => {
    if (!snapshot.project || !selectedAsset || !mediaAssetKinds.has(selectedAsset.kind) || !selectedAsset.probe || selectedAsset.probe.duration_ticks <= 0) {
      uiProjection.patch({ announcement: "BLOCKED — select an available video, audio, or image asset with probe metadata." });
      return;
    }
    if (selectedAsset.status !== "Available") {
      uiProjection.patch({ announcement: "BLOCKED — the selected asset is not available." });
      return;
    }
    const kind = trackKindForAsset(selectedAsset, forcedKind);
    let document = snapshot.project;
    let track = document.sequence.tracks.find((candidate) => candidate.kind === kind && !candidate.locked);
    if (!track) {
      const nextTrack: ProjectTrack = { id: createId(`${kind.toLowerCase()}-layer`), kind, name: `${kind} layer ${document.sequence.tracks.filter((candidate) => candidate.kind === kind).length + 1}`, enabled: true, locked: false, clips: [] };
      const trackResult = await dispatch({ type: "timelineApply", baseRevision: document.revision, operation: { AddTrack: { track: nextTrack } } });
      if (trackResult.status !== "accepted" || !trackResult.snapshot?.project) return;
      document = trackResult.snapshot.project;
      track = document.sequence.tracks.find((candidate) => candidate.id === nextTrack.id);
    }
    if (!track) return;
    const duration = selectedAsset.probe.duration_ticks;
    const timelineStart = track.clips.reduce((end, clip) => Math.max(end, clipEnd(clip)), 0);
    const clip: ProjectClip = {
      id: createId("clip"), asset_id: selectedAsset.id, timeline_start: timelineStart,
      timeline_duration: durationSecondsToTicks(duration * selectedAsset.probe.stream_timebase.denominator / selectedAsset.probe.stream_timebase.numerator, document.sequence.timebase),
      source_start: 0, source_duration: duration, speed: { numerator: 1, denominator: 1 }, opacity: 1,
      transform: { position_x: 0, position_y: 0, scale_x: 1, scale_y: 1, rotation_degrees: 0, anchor_x: 0.5, anchor_y: 0.5 },
      effects: [], keyframes: [], text_overlay: null,
    };
    const result = await dispatch({ type: "timelineApply", baseRevision: document.revision, operation: { AddClip: { track_id: track.id, clip } } });
    if (result.status === "accepted") uiProjection.patch({ selectedClipId: clip.id, announcement: `${assetName(selectedAsset)} added to ${track.name}.` });
  };

  const addTextOverlay = async (text: string) => {
    if (!snapshot.project || !text.trim()) return;
    const textId = createId("title");
    const asset: ProjectAsset = { id: textId, relative_path: `generated/${textId}.title`, kind: "Text", fingerprint: { size_bytes: text.trim().length, modified_time: "generated", sha256: null }, probe: null, status: "Available" };
    let document = snapshot.project;
    let result = await dispatch({ type: "timelineApply", baseRevision: document.revision, operation: { AddAsset: { asset } } });
    if (result.status !== "accepted" || !result.snapshot?.project) return;
    document = result.snapshot.project;
    let track = document.sequence.tracks.find((candidate) => candidate.kind === "Text" && !candidate.locked);
    if (!track) {
      const newTrack: ProjectTrack = { id: createId("text-layer"), kind: "Text", name: "Text layer", enabled: true, locked: false, clips: [] };
      result = await dispatch({ type: "timelineApply", baseRevision: document.revision, operation: { AddTrack: { track: newTrack } } });
      if (result.status !== "accepted" || !result.snapshot?.project) return;
      document = result.snapshot.project;
      track = document.sequence.tracks.find((candidate) => candidate.id === newTrack.id);
    }
    if (!track) return;
    const start = track.clips.reduce((end, clip) => Math.max(end, clipEnd(clip)), 0);
    const duration = durationSecondsToTicks(5, document.sequence.timebase);
    const clip: ProjectClip = { id: createId("text-clip"), asset_id: asset.id, timeline_start: start, timeline_duration: duration, source_start: 0, source_duration: duration, speed: { numerator: 1, denominator: 1 }, opacity: 1, transform: { position_x: 0, position_y: 0, scale_x: 1, scale_y: 1, rotation_degrees: 0, anchor_x: 0.5, anchor_y: 0.5 }, effects: [], keyframes: [], text_overlay: { text: text.trim(), font_size: 56, color: "#FFFFFF", position_x: 0, position_y: 0.7 } };
    result = await dispatch({ type: "timelineApply", baseRevision: document.revision, operation: { AddClip: { track_id: track.id, clip } } });
    if (result.status === "accepted") uiProjection.patch({ selectedAssetId: asset.id, selectedClipId: clip.id, announcement: "Text overlay added to the Text layer." });
  };

  return <div className="app-shell"><header className="topbar"><div className="brand-lockup"><div className="brand-mark"><Icon name="film" size={18} /></div><div><div className="brand-name">CUTLINE</div><div className="brand-subtitle">LOCAL EDITOR</div></div></div><div className="project-context"><span className="project-context-label">PROJECT</span><span className="project-name">{snapshot.project?.name ?? "No project loaded"}</span><StatusPill state={snapshot.connection}>{snapshot.connection}</StatusPill></div><nav className="top-actions" aria-label="Project actions"><button className="button button-quiet" onClick={() => void dispatch({ type: "projectOpenRequested" })}><Icon name="folder" size={15} /> Open</button><button className="button button-quiet" onClick={() => void dispatch({ type: "projectSave" })}><Icon name="save" size={15} /> Save</button><label className="compact-select"><span className="sr-only">Bundle profile</span><select value={bundleProfile} onChange={(event) => setBundleProfile(event.target.value as typeof bundleProfile)} aria-label="Bundle profile"><option value="core">Core media</option><option value="subtitles">Subtitle AI</option><option value="ai">AI tools</option><option value="all">Everything</option></select></label><button className="button button-quiet" disabled={bundleBusy || snapshot.connection !== "READY"} onClick={() => void downloadBundle()}><Icon name="arrowDown" size={15} /> {bundleBusy ? "Downloading…" : "Download bundle"}</button><label className="compact-select"><span className="sr-only">Export profile</span><select value={exportProfile} onChange={(event) => setExportProfile(event.target.value as ExportProfile)} aria-label="Export profile">{exportProfileOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}</select></label><button className="button button-primary" disabled={!snapshot.project} onClick={() => void exportProject()}><Icon name="download" size={15} /> Export</button></nav></header><main className="workspace"><aside className="left-rail" aria-label="Media and project navigation"><section className="panel media-panel"><SectionHeading icon="layout" eyebrow="PROJECT" title="Media bin" /><div className="media-tabs" role="tablist" aria-label="Media categories"><button className={`media-tab ${projection.mediaFilter === "all" ? "is-active" : ""}`} role="tab" aria-selected={projection.mediaFilter === "all"} onClick={() => uiProjection.patch({ mediaFilter: "all" })}>All media <span>{snapshot.project?.assets.length ?? 0}</span></button><button className={`media-tab ${projection.mediaFilter === "used" ? "is-active" : ""}`} role="tab" aria-selected={projection.mediaFilter === "used"} onClick={() => uiProjection.patch({ mediaFilter: "used" })}>Used <span>{snapshot.project?.assets.filter((asset) => snapshot.project?.sequence.tracks.some((track) => track.clips.some((clip) => clip.asset_id === asset.id))).length ?? 0}</span></button></div><div className="media-list" aria-live="polite">{(snapshot.project?.assets.filter((asset) => projection.mediaFilter === "all" || snapshot.project?.sequence.tracks.some((track) => track.clips.some((clip) => clip.asset_id === asset.id))) ?? []).length ? snapshot.project?.assets.filter((asset) => projection.mediaFilter === "all" || snapshot.project?.sequence.tracks.some((track) => track.clips.some((clip) => clip.asset_id === asset.id))).map((asset) => <button key={asset.id} className={`asset-row ${projection.selectedAssetId === asset.id ? "is-selected" : ""}`} onClick={() => uiProjection.patch({ selectedAssetId: asset.id, selectedClipId: null })}><span className="asset-thumb"><Icon name={asset.kind === "Audio" ? "trackAudio" : asset.kind === "Text" || asset.kind === "Subtitle" ? "message" : "film"} size={17} /></span><span className="asset-copy"><strong>{assetName(asset)}</strong><small>{asset.kind.toUpperCase()} · {asset.status}</small></span></button>) : <EmptyState icon="upload" title={projection.mediaFilter === "used" ? "No used media" : "Your bin is empty"} action={projection.mediaFilter === "all" ? <button className="button button-primary button-small" onClick={() => void dispatch({ type: "assetImportRequested" })}><Icon name="plus" size={14} /> Import media</button> : undefined}>{projection.mediaFilter === "used" ? "No real asset is placed on the current timeline." : "Import local video, audio, image, or subtitle files to begin. Nothing is created until a real asset is selected."}</EmptyState>}</div><button className="import-link" onClick={() => void dispatch({ type: "assetImportRequested" })}><Icon name="upload" size={14} /> Import media</button></section><section className="panel project-panel"><SectionHeading icon="folder" eyebrow="WORKSPACE" title="Project" /><button className="project-action" onClick={() => void dispatch({ type: "projectCreate", name: "Untitled project" })}><span className="project-action-icon"><Icon name="plus" size={15} /></span><span><strong>New project</strong><small>Start with an empty document</small></span></button><div className="storage-note"><span className="storage-indicator" /> Local-only workspace <Icon name="circleHelp" size={13} /></div></section></aside><section className="center-stage" aria-label="Editor stage"><section className="panel preview-panel"><SectionHeading icon="play" eyebrow="MONITOR" title="Preview" action={<div className="preview-actions"><span className="quality-label">SOURCE</span><span className="select-button">Fit <Icon name="chevronDown" size={13} /></span></div>} /><div className="preview-canvas" aria-label={selectedAsset ? `Preview for ${assetName(selectedAsset)}` : "Empty preview"}><div className="preview-grid" />{selectedAsset ? <div className="preview-selected"><span className="selected-symbol"><Icon name={selectedAsset.kind === "Audio" ? "trackAudio" : "film"} size={25} /></span><strong>{assetName(selectedAsset)}</strong><span>{selectedAsset.status === "Available" ? "Preview will be provided by the media runtime." : `${selectedAsset.status} asset`}</span></div> : <div className="preview-empty"><div className="preview-orbit"><Icon name="play" size={24} /></div><strong>No media selected</strong><span>Import an asset, then select it from the Media bin.</span></div>}{selectedAsset?.probe?.video && <div className="preview-canvas-meta"><span>{selectedAsset.probe.video.width} × {selectedAsset.probe.video.height}</span></div>}</div><PreviewTransport asset={selectedAsset} projection={projection} dispatch={dispatch} sequenceTimebase={snapshot.project?.sequence.timebase ?? null} /></section><Timeline snapshot={snapshot} tracks={tracks} dispatch={dispatch} /></section><aside className="right-rail" aria-label="Inspector and assistant"><section className="panel inspector-panel"><SectionHeading icon="settings" eyebrow="PROPERTIES" title="Inspector" />{selectedAsset ? <div className="inspector-content"><div className="inspector-title"><span className="inspector-type">{selectedClip ? "CLIP" : selectedAsset.kind.toUpperCase()}</span><strong>{assetName(selectedAsset)}</strong></div><div className="property-list"><Property label="Status" value={selectedAsset.status} /><Property label="Path" value={selectedAsset.relative_path} /><Property label="Fingerprint" value={selectedAsset.fingerprint.sha256 ?? "Not recorded"} />{selectedAsset.probe && <><Property label="Asset duration" value={formatTicks(selectedAsset.probe.duration_ticks, selectedAsset.probe.stream_timebase)} /><Property label="Probe timebase" value={`${selectedAsset.probe.stream_timebase.numerator}/${selectedAsset.probe.stream_timebase.denominator}`} /></>}{selectedClip && <><Property label="Timeline duration" value={formatTicks(selectedClip.timeline_duration, snapshot.project?.sequence.timebase ?? null)} /><Property label="Source duration" value={formatTicks(selectedClip.source_duration, selectedAsset.probe?.stream_timebase ?? null)} /><Property label="Typed effects" value={selectedClip.effects.length ? selectedClip.effects.map(effectName).join(", ") : "None"} /></>}</div></div> : <EmptyState icon="settings" title="Nothing selected">Select a clip or asset to inspect its typed properties.</EmptyState>}</section><EditorControls snapshot={snapshot} selectedAsset={selectedAsset} selectedClip={selectedClip} tracks={tracks} dispatch={dispatch} onAddAsset={() => void addSelectedAssetToTimeline()} onAddOverlay={() => void addSelectedAssetToTimeline("Overlay")} onAddText={addTextOverlay} /><Assistant snapshot={snapshot} projection={projection} dispatch={dispatch} /><Jobs snapshot={snapshot} dispatch={dispatch} /></aside></main>{liveError && <div className="live-error" role="alert" aria-live="assertive">{liveError}</div>}<footer className="statusbar"><div className="statusbar-left"><span className="status-led" /><span role="status" aria-live="polite">{commandStatus}</span></div><div className="statusbar-right"><span><Icon name="clock" size={13} /> Autosave unavailable</span><span><Icon name="activity" size={13} /> CPU effects / local AI</span><span className="help-link" aria-hidden="true"><Icon name="circleHelp" size={15} /></span></div></footer><div className="sr-only" role="status" aria-live="assertive">{projection.announcement}</div></div>;
}

function PreviewTransport({ asset, projection, dispatch, sequenceTimebase }: { asset: ProjectAsset | null; projection: ReturnType<typeof useUiProjection>; dispatch: (command: EditorCommand) => Promise<EditorCommandResult>; sequenceTimebase: Rational | null }) {
  const togglePlayback = async () => {
    const result = await dispatch({ type: projection.isPlaying ? "previewPause" : "previewPlay" });
    if (result.status === "accepted") uiProjection.patch({ isPlaying: !projection.isPlaying });
  };
  const seek = async (direction: -1 | 1) => {
    if (!asset?.probe || !sequenceTimebase) return;
    const next = Math.max(0, projection.playheadTicks + direction);
    const result = await dispatch({ type: "previewSeek", timelineTicks: next });
    if (result.status === "accepted") uiProjection.patch({ playheadTicks: next, announcement: `Playhead at ${formatTicks(next, sequenceTimebase)}.` });
  };
  return <div className="transport" aria-label="Preview controls"><span className="timecode">{formatTicks(projection.playheadTicks, sequenceTimebase)} <span>/ {asset?.probe ? formatTicks(asset.probe.duration_ticks, asset.probe.stream_timebase) : "—:—"}</span></span><div className="transport-center"><button className="icon-button" aria-label="Previous frame" disabled={!asset} onClick={() => void seek(-1)}><Icon name="chevronDown" size={14} className="rotate-90" /></button><button className="play-button" aria-label={projection.isPlaying ? "Pause preview" : "Play preview"} disabled={!asset} onClick={() => void togglePlayback()}><Icon name={projection.isPlaying ? "pause" : "play"} size={15} /></button><button className="icon-button" aria-label="Next frame" disabled={!asset} onClick={() => void seek(1)}><Icon name="chevronDown" size={14} className="rotate-270" /></button></div><span className="icon-status" aria-label="Volume controls unavailable"><Icon name="volume" size={16} /></span></div>;
}
function Property({ label, value }: { label: string; value: string }) { return <div className="property-row"><span>{label}</span><strong title={value}>{value}</strong></div>; }

function Timeline({ snapshot, tracks, dispatch }: { snapshot: EditorSnapshot; tracks: ProjectTrack[]; dispatch: (command: EditorCommand) => Promise<EditorCommandResult> }) {
  const projection = useUiProjection();
  const timebase = snapshot.project?.sequence.timebase ?? null;
  const visibleTracks = tracks.length ? tracks : [{ id: "video-empty", kind: "Video" as const, name: "Video", enabled: true, locked: false, clips: [] }, { id: "overlay-empty", kind: "Overlay" as const, name: "Overlay", enabled: true, locked: false, clips: [] }, { id: "audio-empty", kind: "Audio" as const, name: "Audio", enabled: true, locked: false, clips: [] }, { id: "text-empty", kind: "Text" as const, name: "Text", enabled: true, locked: false, clips: [] }];
  const clipDuration = tracks.flatMap((track) => track.clips).reduce((end, clip) => Math.max(end, clipEnd(clip)), 0);
  const timelineDuration = Math.max(1, clipDuration, timebase ? durationSecondsToTicks(30, timebase) : 30);
  const markers = snapshot.project?.sequence.markers ?? [];
  const selectedClip = tracks.flatMap((track) => track.clips).find((clip) => clip.id === projection.selectedClipId) ?? null;
  const handleTimelineKeyDown = async (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === " ") { event.preventDefault(); const result = await dispatch({ type: projection.isPlaying ? "previewPause" : "previewPlay" }); if (result.status === "accepted") uiProjection.patch({ isPlaying: !projection.isPlaying }); return; }
    if (event.key === "ArrowRight" || event.key === "ArrowLeft") { event.preventDefault(); const next = Math.max(0, projection.playheadTicks + (event.key === "ArrowRight" ? 1 : -1)); const result = await dispatch({ type: "previewSeek", timelineTicks: next }); if (result.status === "accepted") uiProjection.patch({ playheadTicks: next, announcement: `Playhead at ${formatTicks(next, timebase)}.` }); return; }
    if (event.key === "Delete" && snapshot.project && selectedClip) { event.preventDefault(); const track = tracks.find((candidate) => candidate.clips.some((clip) => clip.id === selectedClip.id)); if (track) await dispatch({ type: "timelineApply", baseRevision: snapshot.project.revision, operation: event.shiftKey ? { RippleDelete: { track_id: track.id, clip_id: selectedClip.id } } : { DeleteClip: { clip_id: selectedClip.id } } }); return; }
    if (event.key.toLowerCase() === "s" && !event.ctrlKey && !event.altKey && !event.metaKey && snapshot.project && selectedClip) { event.preventDefault(); const end = clipEnd(selectedClip); if (projection.playheadTicks > selectedClip.timeline_start && projection.playheadTicks < end) await dispatch({ type: "timelineApply", baseRevision: snapshot.project.revision, operation: { SplitClip: { clip_id: selectedClip.id, at_timeline_tick: projection.playheadTicks } } }); else uiProjection.patch({ announcement: "BLOCKED — move playhead inside selected clip before splitting." }); }
  };
  return <section className="panel timeline-panel"><div className="timeline-header"><SectionHeading icon="scissors" eyebrow="SEQUENCE" title="Timeline" action={<div className="timeline-tools"><span className="zoom-label">Magnetic snap on</span></div>} /></div><div className="marker-strip"><span className="marker-label"><Icon name="marker" size={13} /> MARKERS</span>{markers.length ? <div className="marker-list">{markers.map((marker) => <span key={marker.id} className="marker-item" title={marker.comment ?? undefined} style={{ borderColor: marker.color_tag ?? undefined }}>{marker.name} · {formatTicks(marker.position_ticks, timebase)}</span>)}</div> : <div className="marker-empty">No markers yet <span>Add one from the editing controls.</span></div>}</div><div className="timeline-scroll" tabIndex={0} onKeyDown={(event) => void handleTimelineKeyDown(event)} role="region" aria-label="Timeline. Drag clips between layers, use arrows to seek, Delete to delete, Shift+Delete to ripple delete, S to split, and Space to play or pause."><div className="timeline-ruler" style={{ minWidth: `${Math.max(610, timelineDuration * 2)}px` }}><div className="track-label-spacer" />{[0, .25, .5, .75].map((fraction) => <span key={fraction}>{formatTicks(Math.round(timelineDuration * fraction), timebase)}</span>)}</div>{visibleTracks.map((track) => <TrackLane key={track.id} track={track} timebase={timebase} timelineDuration={timelineDuration} allClips={tracks.flatMap((candidate) => candidate.clips)} selectedClipId={projection.selectedClipId} baseRevision={snapshot.project?.revision ?? null} editable={tracks.some((candidate) => candidate.id === track.id)} dispatch={dispatch} onSelect={(clip) => uiProjection.patch({ selectedClipId: clip.id, selectedAssetId: clip.asset_id, announcement: `Selected clip ${clip.id}.` })} />)}{!snapshot.project && <div className="timeline-empty-note"><Icon name="layout" size={16} /> Timeline is ready for a real project document.</div>}</div><div className="timeline-footer"><span><kbd>←</kbd><kbd>→</kbd> Seek</span><span><kbd>Space</kbd> Play / pause</span><span><kbd>Delete</kbd> Delete</span><span><kbd>Shift+Delete</kbd> Ripple</span><span><kbd>S</kbd> Split</span><span className="timeline-footer-right">{clipDuration ? formatTicks(clipDuration, timebase) : "No duration available"}</span></div></section>;
}

function TrackLane({ track, timebase, timelineDuration, allClips, selectedClipId, baseRevision, editable, dispatch, onSelect }: { track: ProjectTrack; timebase: Rational | null; timelineDuration: number; allClips: ProjectClip[]; selectedClipId: string | null; baseRevision: number | null; editable: boolean; dispatch: (command: EditorCommand) => Promise<EditorCommandResult>; onSelect: (clip: ProjectClip) => void }) {
  const canvasRef = useRef<HTMLDivElement>(null);
  const setTrackState = (field: "enabled" | "locked") => { if (baseRevision !== null && editable) void dispatch({ type: "timelineApply", baseRevision, operation: { SetTrackState: { track_id: track.id, [field]: !track[field] } } }); };
  const snap = (tick: number) => {
    const threshold = timebase ? durationSecondsToTicks(0.3, timebase) : 9;
    const candidates = [0, ...allClips.flatMap((clip) => [clip.timeline_start, clipEnd(clip)])];
    const nearest = candidates.reduce((best, candidate) => Math.abs(candidate - tick) < Math.abs(best - tick) ? candidate : best, tick);
    return Math.abs(nearest - tick) <= threshold ? nearest : tick;
  };
  const dropClip = (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault();
    if (!editable || baseRevision === null || track.locked) return;
    const raw = event.dataTransfer.getData("application/x-videoeditor-clip");
    if (!raw || !canvasRef.current) return;
    try {
      const payload = JSON.parse(raw) as { clipId: string; trackId: string };
      const rect = canvasRef.current.getBoundingClientRect();
      const ratio = Math.max(0, Math.min(1, (event.clientX - rect.left) / Math.max(rect.width, 1)));
      const current = allClips.find((clip) => clip.id === payload.clipId);
      const tick = Math.max(0, Math.round(ratio * timelineDuration) - (current?.timeline_duration ?? 0) / 2);
      const target = snap(tick);
      const operation = payload.trackId === track.id ? { MoveClip: { clip_id: payload.clipId, timeline_start: target } } : { MoveClipToTrack: { clip_id: payload.clipId, track_id: track.id, timeline_start: target } };
      void dispatch({ type: "timelineApply", baseRevision, operation });
    } catch { uiProjection.patch({ announcement: "BLOCKED — the dropped timeline item is invalid." }); }
  };
  const icon: IconName = track.kind === "Audio" ? "trackAudio" : track.kind === "Video" || track.kind === "Overlay" ? "trackVideo" : "message";
  return <div className={`track-lane ${track.enabled ? "" : "is-disabled"}`}><div className="track-label"><Icon name={icon} size={15} /><span title={track.name}>{track.name}</span>{editable && <span className="track-actions"><button className="track-control" type="button" aria-label={`${track.enabled ? "Disable" : "Enable"} ${track.name}`} onClick={() => setTrackState("enabled")}>{track.enabled ? "On" : "Off"}</button><button className="track-control" type="button" aria-label={`${track.locked ? "Unlock" : "Lock"} ${track.name}`} onClick={() => setTrackState("locked")}><Icon name={track.locked ? "lock" : "check"} size={12} /></button></span>}</div><div ref={canvasRef} className="track-canvas" style={{ minWidth: `${Math.max(495, timelineDuration * 2)}px` }} onDragOver={(event) => event.preventDefault()} onDrop={dropClip}>{track.clips.length === 0 ? <span className="lane-empty">Drop a clip here</span> : track.clips.map((clip) => <button className={`timeline-clip ${selectedClipId === clip.id ? "is-selected" : ""}`} style={{ left: `${(clip.timeline_start / timelineDuration) * 100}%`, width: `${Math.max(1.5, (clip.timeline_duration / timelineDuration) * 100)}%` }} key={clip.id} type="button" draggable={!track.locked && editable} aria-pressed={selectedClipId === clip.id} aria-label={`Select clip ${clip.id}, timeline duration ${formatTicks(clip.timeline_duration, timebase)}`} onDragStart={(event) => event.dataTransfer.setData("application/x-videoeditor-clip", JSON.stringify({ clipId: clip.id, trackId: track.id }))} onClick={() => onSelect(clip)}>{clip.text_overlay?.text ?? clip.id}</button>)}</div></div>;
}

function EditorControls({ snapshot, selectedAsset, selectedClip, tracks, dispatch, onAddAsset, onAddOverlay, onAddText }: { snapshot: EditorSnapshot; selectedAsset: ProjectAsset | null; selectedClip: ProjectClip | null; tracks: ProjectTrack[]; dispatch: (command: EditorCommand) => Promise<EditorCommandResult>; onAddAsset: () => void; onAddOverlay: () => void; onAddText: (text: string) => Promise<void> }) {
  const [subtitleLanguage, setSubtitleLanguage] = useState("auto");
  const [subtitleBusy, setSubtitleBusy] = useState(false);
  const [ttsText, setTtsText] = useState("");
  const [ttsBusy, setTtsBusy] = useState(false);
  const [titleText, setTitleText] = useState("");
  const [effectIndex, setEffectIndex] = useState(0);
  const [lutPath, setLutPath] = useState("looks/custom.cube");
  const [markerName, setMarkerName] = useState("");
  const [markerComment, setMarkerComment] = useState("");
  const [markerColor, setMarkerColor] = useState("#e9b56b");
  const [duckSourceId, setDuckSourceId] = useState("");
  const [duckTargetId, setDuckTargetId] = useState("");
  const project = snapshot.project;
  const projection = useUiProjection();
  const audioTracks = tracks.filter((track) => track.kind === "Audio");
  const subtitleCapability = snapshot.capabilities.subtitles;
  const ttsCapability = snapshot.capabilities.tts;
  const sourceId = audioTracks.some((track) => track.id === duckSourceId) ? duckSourceId : audioTracks[0]?.id ?? "";
  const targetId = audioTracks.some((track) => track.id === duckTargetId && track.id !== sourceId) ? duckTargetId : audioTracks.find((track) => track.id !== sourceId)?.id ?? "";
  const targetTrack = audioTracks.find((track) => track.id === targetId);
  const subtitleTrackId = tracks.find((track) => track.kind === "Subtitle" && !track.locked)?.id;
  const canGenerateSubtitles = Boolean(project && selectedAsset && (selectedAsset.kind === "Video" || selectedAsset.kind === "Audio") && subtitleCapability.state === "READY" && !subtitleBusy);
  const canGenerateTts = Boolean(project && ttsText.trim() && ttsCapability.state === "READY" && !ttsBusy);
  const generateSubtitles = async () => { if (!project || !selectedAsset || !canGenerateSubtitles) return; setSubtitleBusy(true); try { await dispatch({ type: "subtitleGenerate", assetId: selectedAsset.id, language: subtitleLanguage, baseRevision: project.revision, trackId: subtitleTrackId }); } finally { setSubtitleBusy(false); } };
  const generateTts = async () => { if (!project || !canGenerateTts) return; setTtsBusy(true); try { const result = await dispatch({ type: "ttsGenerate", text: ttsText.trim(), voiceId: "en_US-lessac-medium", baseRevision: project.revision }); if (result.status === "accepted") setTtsText(""); } finally { setTtsBusy(false); } };
  const addEffect = () => { if (!project || !selectedClip) return; const preset = effectPresets[effectIndex]?.value; if (!preset) return; const effect: Effect = "Lut" in preset ? { Lut: { relative_path: lutPath.trim() } } : preset; if ("Lut" in effect && !effect.Lut.relative_path.trim()) return; void dispatch({ type: "timelineApply", baseRevision: project.revision, operation: { SetClipEffects: { clip_id: selectedClip.id, effects: [...selectedClip.effects, effect] } } }); };
  const removeEffect = (index: number) => { if (project && selectedClip) void dispatch({ type: "timelineApply", baseRevision: project.revision, operation: { SetClipEffects: { clip_id: selectedClip.id, effects: selectedClip.effects.filter((_, effectIndex) => effectIndex !== index) } } }); };
  const trimBySecond = (direction: -1 | 1) => { if (!project || !selectedClip || !selectedAsset?.probe) return; const step = durationSecondsToTicks(1, selectedAsset.probe.stream_timebase); const sourceStart = direction < 0 ? selectedClip.source_start + step : selectedClip.source_start; const sourceEnd = direction < 0 ? selectedClip.source_start + selectedClip.source_duration : selectedClip.source_start + selectedClip.source_duration - step; if (sourceEnd > sourceStart) void dispatch({ type: "timelineApply", baseRevision: project.revision, operation: { TrimClip: { clip_id: selectedClip.id, source_start: sourceStart, source_end: sourceEnd } } }); };
  const setOpacity = (opacity: number) => { if (project && selectedClip) void dispatch({ type: "timelineApply", baseRevision: project.revision, operation: { SetClipVisuals: { clip_id: selectedClip.id, opacity, transform: selectedClip.transform } } }); };
  const addMarker = () => { if (!project || !markerName.trim()) return; void dispatch({ type: "timelineApply", baseRevision: project.revision, operation: { AddMarker: { marker: { id: createId("marker"), position_ticks: projection.playheadTicks, name: markerName.trim(), comment: markerComment.trim() || null, color_tag: markerColor, clip_id: selectedClip?.id ?? null } } } }); setMarkerName(""); setMarkerComment(""); };
  const toggleDucking = () => { if (project && targetTrack && sourceId && sourceId !== targetId && !targetTrack.locked) void dispatch({ type: "timelineApply", baseRevision: project.revision, operation: { SetTrackDucking: { track_id: targetId, ducking: targetTrack.ducking ? null : { source_track_id: sourceId, threshold_db: -24, ratio: 6, attack_ms: 20, release_ms: 300 } } } }); };
  return <section className="panel controls-panel"><SectionHeading icon="layout" eyebrow="EDITING" title="Layers & automation" /><div className="control-group"><div className="control-heading"><strong>Selected layer</strong><span>{selectedAsset ? assetName(selectedAsset) : "No asset"}</span></div><div className="control-actions"><button className="button button-primary button-small" disabled={!selectedAsset || selectedAsset.status !== "Available" || !selectedAsset.probe || selectedAsset.probe.duration_ticks <= 0} onClick={onAddAsset}><Icon name="plus" size={14} /> Add video/audio</button><button className="button button-quiet button-small" disabled={!selectedAsset || selectedAsset.kind === "Audio" || selectedAsset.kind === "Subtitle" || selectedAsset.kind === "Text"} onClick={onAddOverlay}>Add overlay</button></div><small className="control-help">Drag clips between editable layers; drops magnetically snap to clip edges and zero.</small></div><div className="control-group"><div className="control-heading"><strong>Text overlay</strong><span>Text layer</span></div><input className="control-input" value={titleText} onChange={(event) => setTitleText(event.target.value)} placeholder="Type a title or callout" maxLength={4096} /><button className="button button-quiet button-small" disabled={!project || !titleText.trim()} onClick={() => { void onAddText(titleText); setTitleText(""); }}><Icon name="message" size={14} /> Add text layer</button></div><div className="control-group"><div className="control-heading"><strong>Trim & layer visuals</strong><span>{selectedClip ? formatTicks(selectedClip.timeline_duration, project?.sequence.timebase ?? null) : "No clip"}</span></div><div className="control-actions"><button className="button button-quiet button-small" disabled={!selectedClip} onClick={() => trimBySecond(-1)}>Trim 1s start</button><button className="button button-quiet button-small" disabled={!selectedClip} onClick={() => trimBySecond(1)}>Trim 1s end</button></div><label className="control-label" htmlFor="clip-opacity">Opacity {selectedClip ? `${Math.round(selectedClip.opacity * 100)}%` : "—"}</label><input id="clip-opacity" type="range" min="0" max="1" step="0.01" value={selectedClip?.opacity ?? 1} disabled={!selectedClip} onChange={(event) => setOpacity(Number(event.target.value))} /><small className="control-help">Operations use integer project ticks and half-open clip ranges.</small></div><div className="control-group"><div className="control-heading"><strong>Effects & color</strong><StatusPill state={snapshot.capabilities.effects.state}>{snapshot.capabilities.effects.state}</StatusPill></div><div className="control-actions"><select className="control-select" value={effectIndex} onChange={(event) => setEffectIndex(Number(event.target.value))}>{effectPresets.map((preset, index) => <option value={index} key={preset.label}>{preset.label}</option>)}</select><button className="button button-quiet button-small" disabled={!selectedClip} onClick={addEffect}>Add effect</button></div>{effectPresets[effectIndex]?.value && "Lut" in effectPresets[effectIndex].value && <><label className="control-label" htmlFor="lut-path">Project-relative .cube path</label><input id="lut-path" className="control-input" value={lutPath} onChange={(event) => setLutPath(event.target.value)} placeholder="looks/custom.cube" /><small className="control-help">The LUT file must already exist inside the project root; the export planner rejects missing or escaping paths.</small></>}{selectedClip?.effects.length ? <div className="effect-list">{selectedClip.effects.map((effect, index) => <span className="effect-chip" key={`${effectName(effect)}-${index}`}>{effectName(effect)} <button type="button" onClick={() => removeEffect(index)} aria-label={`Remove ${effectName(effect)}`}>×</button></span>)}</div> : <small className="control-help">Includes exposure, gamma, white balance, three-way balance, blur, sharpen, vignette, duotone, 3D LUT, and fades.</small>}</div><div className="control-group"><div className="control-heading"><strong>Auto subtitles</strong><StatusPill state={subtitleCapability.state}>{subtitleCapability.state}</StatusPill></div><label className="control-label" htmlFor="subtitle-language">Spoken language</label><select id="subtitle-language" className="control-select" value={subtitleLanguage} onChange={(event) => setSubtitleLanguage(event.target.value)}>{subtitleLanguages.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select><button className="button button-quiet button-small" disabled={!canGenerateSubtitles} onClick={() => void generateSubtitles()}><Icon name="message" size={14} /> {subtitleBusy ? "Generating…" : "Generate subtitles"}</button><small className="control-help">Whisper.cpp runs locally and adds synchronized cues to a subtitle layer. Pick the spoken language; translation is not fabricated.</small></div><div className="control-group"><div className="control-heading"><strong>Local voiceover</strong><StatusPill state={ttsCapability.state}>{ttsCapability.state}</StatusPill></div><textarea className="control-textarea" value={ttsText} onChange={(event) => setTtsText(event.target.value)} placeholder="Write voiceover text…" rows={3} maxLength={8192} /><button className="button button-quiet button-small" disabled={!canGenerateTts} onClick={() => void generateTts()}><Icon name="volume" size={14} /> {ttsBusy ? "Synthesizing…" : "Generate Piper voiceover"}</button><small className="control-help">Uses the verified local en_US-lessac-medium voice and inserts a real WAV clip into an audio layer.</small></div><div className="control-group"><div className="control-heading"><strong>Audio ducking</strong><StatusPill state={snapshot.capabilities.audioDucking.state}>{snapshot.capabilities.audioDucking.state}</StatusPill></div>{audioTracks.length < 2 ? <small className="control-help">Add two audio layers to enable sidechain ducking.</small> : <><label className="control-label" htmlFor="duck-source">Ducking source</label><select id="duck-source" className="control-select" value={sourceId} onChange={(event) => setDuckSourceId(event.target.value)}>{audioTracks.map((track) => <option key={track.id} value={track.id}>{track.name}</option>)}</select><label className="control-label" htmlFor="duck-target">Lower this layer</label><select id="duck-target" className="control-select" value={targetId} onChange={(event) => setDuckTargetId(event.target.value)}>{audioTracks.filter((track) => track.id !== sourceId).map((track) => <option key={track.id} value={track.id}>{track.name}</option>)}</select><button className="button button-quiet button-small" disabled={snapshot.capabilities.audioDucking.state !== "READY" || !targetTrack || targetTrack.locked || sourceId === targetId} onClick={toggleDucking}><Icon name="volume" size={14} /> {targetTrack?.ducking ? "Disable ducking" : "Enable ducking" }</button><small className="control-help">The speech source sidechains music with FFmpeg sidechaincompress during export.</small></>}</div><div className="control-group"><div className="control-heading"><strong>Timeline marker</strong><span>{formatTicks(projection.playheadTicks, project?.sequence.timebase ?? null)}</span></div><input className="control-input" value={markerName} onChange={(event) => setMarkerName(event.target.value)} placeholder="Marker name" maxLength={128} /><input className="control-input" value={markerComment} onChange={(event) => setMarkerComment(event.target.value)} placeholder="Comment (optional)" maxLength={1024} /><input className="control-color" type="color" value={markerColor} onChange={(event) => setMarkerColor(event.target.value)} aria-label="Marker color" /><button className="button button-quiet button-small" disabled={!project || !markerName.trim()} onClick={addMarker}><Icon name="marker" size={14} /> Add marker at playhead</button></div></section>;
}

function Assistant({ snapshot, projection, dispatch }: { snapshot: EditorSnapshot; projection: ReturnType<typeof useUiProjection>; dispatch: (command: EditorCommand) => Promise<EditorCommandResult> }) {
  const [draft, setDraft] = useState(projection.assistantDraft);
  const [plan, setPlan] = useState<AssistantPlanResponse | null>(null);
  const [busy, setBusy] = useState(false);
  const assistant = snapshot.capabilities.assistant;
  const submit = async (event: FormEvent) => { event.preventDefault(); if (!draft.trim() || !snapshot.project || busy) return; setBusy(true); try { const result = await dispatch({ type: "assistantPlan", baseRevision: snapshot.project.revision, text: draft.trim() }); if (result.status === "accepted" && result.assistant) setPlan(result.assistant); } finally { setBusy(false); } };
  const apply = async () => { if (!plan) return; const result = await dispatch({ type: "assistantApply", plan: plan.plan }); if (result.status === "accepted") setPlan(null); };
  return <section className="panel assistant-panel"><SectionHeading icon="spark" eyebrow="ASSISTANT" title="Edit assistant" action={<StatusPill state={assistant.state}>{assistant.state}</StatusPill>} /><div className="assistant-unavailable"><div className="assistant-icon"><Icon name="spark" size={19} /></div><div><strong>{assistant.state === "READY" ? "Local plan generation is ready" : "Local plan generation is unavailable"}</strong><p>{assistant.reason} Prompts stay local; plans are validated and require confirmation.</p></div></div>{plan && <div className="assistant-plan"><strong>{plan.message}</strong><small>{plan.provenance.provider} · {plan.provenance.model_id}</small><div className="plan-operations">{plan.plan.operations.map((operation, index) => <code key={index}>{JSON.stringify(operation)}</code>)}</div><div className="control-actions"><button className="button button-primary button-small" onClick={() => void apply()}>Apply plan</button><button className="button button-quiet button-small" onClick={() => setPlan(null)}>Discard</button></div></div>}<form className="assistant-form" onSubmit={(event) => void submit(event)}><label htmlFor="assistant-prompt">Describe an edit</label><div className="assistant-input-wrap"><textarea id="assistant-prompt" value={draft} onChange={(event) => { setDraft(event.target.value); uiProjection.patch({ assistantDraft: event.target.value }); }} placeholder="e.g. split the selected clip at 00:10" rows={3} /><button className="send-button" type="submit" aria-label="Request edit plan" disabled={!draft.trim() || !snapshot.project || assistant.state !== "READY" || busy}><Icon name="send" size={15} /></button></div><small>{busy ? "Generating a bounded local plan…" : "Reviewable operations only; no shell, path, network, or raw filtergraph actions."}</small></form></section>;
}

function Jobs({ snapshot, dispatch }: { snapshot: EditorSnapshot; dispatch: (command: EditorCommand) => Promise<EditorCommandResult> }) {
  const stateToStatus = (state: string): "READY" | "BLOCKED" | "UNAVAILABLE" => state === "succeeded" ? "READY" : state === "cancelled" ? "UNAVAILABLE" : "BLOCKED";
  return <section className="panel jobs-panel"><SectionHeading icon="activity" eyebrow="ACTIVITY" title="Jobs" action={<span className="job-count">{snapshot.jobs.length}</span>} />{snapshot.jobs.length ? snapshot.jobs.map((job) => <div className="job-row" key={job.id}><div className="job-heading"><span>{job.kind}</span><StatusPill state={stateToStatus(job.state)}>{job.state}</StatusPill></div><p>{job.error?.message ?? job.message}</p>{job.progress !== null && <progress value={job.progress} max={1} aria-label={`${job.kind} progress`} />}{job.state === "running" && <button className="button button-small" onClick={() => void dispatch({ type: "jobCancel", jobId: job.id })}>Cancel</button>}</div>) : <div className="jobs-empty"><Icon name="clock" size={16} /><span>No jobs reported.<small>Probe, subtitle, voiceover, render, and export status will appear here.</small></span></div>}</section>;
}

export default App;
