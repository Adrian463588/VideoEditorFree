import { useEffect, useMemo, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import { editorFacade, type EditorCommand, type EditorCommandResult } from "./api/editorFacade";
import { Icon, type IconName } from "./components/Icon";
import { uiProjection, useUiProjection } from "./stores/uiProjection";
import { emptyEditorSnapshot, type EditorSnapshot, type ProjectAsset, type ProjectClip, type ProjectTrack, type Rational } from "./types/editor";

const formatTicks = (ticks: number, timebase: Rational | null) => {
  if (!timebase || timebase.numerator <= 0 || timebase.denominator <= 0) return "—:—";
  const seconds = Math.max(0, Math.floor((ticks * timebase.denominator) / timebase.numerator));
  return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
};

const assetName = (asset: ProjectAsset) => asset.relative_path.split(/[\\/]/).pop() ?? asset.relative_path;
const effectName = (effect: ProjectClip["effects"][number]) => Object.keys(effect)[0] ?? "Unknown";
const errorDetail = (error: unknown) => error instanceof Error ? error.message : typeof error === "string" ? error : "Unknown host error.";

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
    const loadSnapshot = async () => {
      try {
        const next = await editorFacade.getSnapshot();
        if (active) {
          setSnapshot(next);
          setCommandStatus(next.connectionMessage);
        }
      } catch (error) {
        if (active) reportError("Loading editor host status", error);
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
      if (result.status === "accepted" && result.snapshot) {
        setSnapshot(result.snapshot);
      }
      if (result.status === "accepted" && (command.type === "projectCreate" || command.type === "projectOpenRequested")) {
        uiProjection.patch({ selectedAssetId: null, selectedClipId: null, playheadTicks: 0, isPlaying: false });
      }
      if (result.status === "accepted" && command.type === "timelineApply" && "DeleteClip" in command.operation) {
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
  const openProject = () => {
    void dispatch({ type: "projectOpenRequested" });
  };
  const exportProject = () => {
    const outputPath = window.prompt("Project-relative output path", "output.mp4");
    if (outputPath?.trim()) void dispatch({ type: "export", outputPath: outputPath.trim(), baseRevision: snapshot.project?.revision });
  };
  const downloadBundle = async () => {
    setBundleBusy(true);
    try {
      await dispatch({ type: "bundleDownload", profile: "core" });
    } finally {
      setBundleBusy(false);
    }
  };

  return <div className="app-shell">
    <header className="topbar"><div className="brand-lockup"><div className="brand-mark"><Icon name="film" size={18} /></div><div><div className="brand-name">CUTLINE</div><div className="brand-subtitle">LOCAL EDITOR</div></div></div><div className="project-context"><span className="project-context-label">PROJECT</span><span className="project-name">{snapshot.project?.name ?? "No project loaded"}</span><StatusPill state={snapshot.connection}>{snapshot.connection}</StatusPill></div><nav className="top-actions" aria-label="Project actions"><button className="button button-quiet" onClick={openProject}><Icon name="folder" size={15} /> Open</button><button className="button button-quiet" onClick={() => void dispatch({ type: "projectSave" })}><Icon name="save" size={15} /> Save</button><button className="button button-quiet" disabled={bundleBusy || snapshot.connection !== "READY"} onClick={() => void downloadBundle()}><Icon name="arrowDown" size={15} /> {bundleBusy ? "Downloading…" : "Download bundle"}</button><button className="button button-primary" onClick={exportProject}><Icon name="download" size={15} /> Export</button></nav></header>
    <main className="workspace">
      <aside className="left-rail" aria-label="Media and project navigation"><section className="panel media-panel"><SectionHeading icon="layout" eyebrow="PROJECT" title="Media bin" /><div className="media-tabs" role="tablist" aria-label="Media categories"><button className={`media-tab ${projection.mediaFilter === "all" ? "is-active" : ""}`} role="tab" aria-selected={projection.mediaFilter === "all"} onClick={() => uiProjection.patch({ mediaFilter: "all" })}>All media <span>{snapshot.project?.assets.length ?? 0}</span></button><button className={`media-tab ${projection.mediaFilter === "used" ? "is-active" : ""}`} role="tab" aria-selected={projection.mediaFilter === "used"} onClick={() => uiProjection.patch({ mediaFilter: "used" })}>Used <span>{snapshot.project?.assets.filter((asset) => snapshot.project?.sequence.tracks.some((track) => track.clips.some((clip) => clip.asset_id === asset.id))).length ?? 0}</span></button></div><div className="media-list" aria-live="polite">{(snapshot.project?.assets.filter((asset) => projection.mediaFilter === "all" || snapshot.project?.sequence.tracks.some((track) => track.clips.some((clip) => clip.asset_id === asset.id))) ?? []).length ? snapshot.project?.assets.filter((asset) => projection.mediaFilter === "all" || snapshot.project?.sequence.tracks.some((track) => track.clips.some((clip) => clip.asset_id === asset.id))).map((asset) => <button key={asset.id} className={`asset-row ${projection.selectedAssetId === asset.id ? "is-selected" : ""}`} onClick={() => uiProjection.patch({ selectedAssetId: asset.id, selectedClipId: null })}><span className="asset-thumb"><Icon name={asset.kind === "Audio" ? "trackAudio" : "film"} size={17} /></span><span className="asset-copy"><strong>{assetName(asset)}</strong><small>{asset.kind.toUpperCase()} · {asset.status}</small></span></button>) : <EmptyState icon="upload" title={projection.mediaFilter === "used" ? "No used media" : "Your bin is empty"} action={projection.mediaFilter === "all" ? <button className="button button-primary button-small" onClick={() => void dispatch({ type: "assetImportRequested" })}><Icon name="plus" size={14} /> Import media</button> : undefined}>{projection.mediaFilter === "used" ? "No real asset is placed on the current timeline." : "Import local video, audio, image, or subtitle files to begin. Nothing is created until a real asset is selected."}</EmptyState>}</div><button className="import-link" onClick={() => void dispatch({ type: "assetImportRequested" })}><Icon name="upload" size={14} /> Import media</button></section><section className="panel project-panel"><SectionHeading icon="folder" eyebrow="WORKSPACE" title="Project" /><button className="project-action" onClick={() => void dispatch({ type: "projectCreate", name: "Untitled project" })}><span className="project-action-icon"><Icon name="plus" size={15} /></span><span><strong>New project</strong><small>Start with an empty document</small></span></button><div className="storage-note"><span className="storage-indicator" /> Local-only workspace <Icon name="circleHelp" size={13} /></div></section></aside>
      <section className="center-stage" aria-label="Editor stage"><section className="panel preview-panel"><SectionHeading icon="play" eyebrow="MONITOR" title="Preview" action={<div className="preview-actions"><span className="quality-label">SOURCE</span><span className="select-button">Fit <Icon name="chevronDown" size={13} /></span></div>} /><div className="preview-canvas" aria-label={selectedAsset ? `Preview for ${assetName(selectedAsset)}` : "Empty preview"}><div className="preview-grid" />{selectedAsset ? <div className="preview-selected"><span className="selected-symbol"><Icon name="film" size={25} /></span><strong>{assetName(selectedAsset)}</strong><span>{selectedAsset.status === "Available" ? "Preview will be provided by the media runtime." : `${selectedAsset.status} asset`}</span></div> : <div className="preview-empty"><div className="preview-orbit"><Icon name="play" size={24} /></div><strong>No media selected</strong><span>Import an asset, then select it from the Media bin.</span></div>}{selectedAsset?.probe?.video && <div className="preview-canvas-meta"><span>{selectedAsset.probe.video.width} × {selectedAsset.probe.video.height}</span></div>}</div><PreviewTransport asset={selectedAsset} projection={projection} dispatch={dispatch} sequenceTimebase={snapshot.project?.sequence.timebase ?? null} /></section><Timeline snapshot={snapshot} tracks={tracks} dispatch={dispatch} /></section>
      <aside className="right-rail" aria-label="Inspector and assistant"><section className="panel inspector-panel"><SectionHeading icon="settings" eyebrow="PROPERTIES" title="Inspector" />{selectedAsset ? <div className="inspector-content"><div className="inspector-title"><span className="inspector-type">{selectedClip ? "CLIP" : selectedAsset.kind.toUpperCase()}</span><strong>{assetName(selectedAsset)}</strong></div><div className="property-list"><Property label="Status" value={selectedAsset.status} /><Property label="Path" value={selectedAsset.relative_path} /><Property label="Fingerprint" value={selectedAsset.fingerprint.sha256 ?? "Not recorded"} />{selectedAsset.probe && <><Property label="Asset duration" value={formatTicks(selectedAsset.probe.duration_ticks, selectedAsset.probe.stream_timebase)} /><Property label="Probe timebase" value={`${selectedAsset.probe.stream_timebase.numerator}/${selectedAsset.probe.stream_timebase.denominator}`} /></>}{selectedClip && <><Property label="Timeline duration" value={formatTicks(selectedClip.timeline_duration, snapshot.project?.sequence.timebase ?? null)} /><Property label="Source duration" value={formatTicks(selectedClip.source_duration, selectedAsset.probe?.stream_timebase ?? null)} /><Property label="Typed effects" value={selectedClip.effects.length ? selectedClip.effects.map(effectName).join(", ") : "None"} /></>}</div></div> : <EmptyState icon="settings" title="Nothing selected">Select a clip or asset to inspect its typed properties.</EmptyState>}</section><Assistant snapshot={snapshot} projection={projection} dispatch={dispatch} /><Jobs snapshot={snapshot} dispatch={dispatch} /></aside>
    </main>{liveError && <div className="live-error" role="alert" aria-live="assertive">{liveError}</div>}<footer className="statusbar"><div className="statusbar-left"><span className="status-led" /><span role="status" aria-live="polite">{commandStatus}</span></div><div className="statusbar-right"><span><Icon name="clock" size={13} /> Autosave unavailable</span><span><Icon name="activity" size={13} /> CPU baseline</span><span className="help-link" aria-hidden="true"><Icon name="circleHelp" size={15} /></span></div></footer><div className="sr-only" role="status" aria-live="assertive">{projection.announcement}</div>
  </div>;
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
  const duration = asset?.probe?.duration_ticks ?? null;
  return <div className="transport" aria-label="Preview controls"><span className="timecode">{formatTicks(projection.playheadTicks, sequenceTimebase)} <span>/ {duration === null ? "—:—" : formatTicks(duration, asset?.probe?.stream_timebase ?? null)}</span></span><div className="transport-center"><button className="icon-button" aria-label="Previous frame" disabled={!asset} onClick={() => void seek(-1)}><Icon name="chevronDown" size={14} className="rotate-90" /></button><button className="play-button" aria-label={projection.isPlaying ? "Pause preview" : "Play preview"} disabled={!asset} onClick={() => void togglePlayback()}><Icon name={projection.isPlaying ? "pause" : "play"} size={15} /></button><button className="icon-button" aria-label="Next frame" disabled={!asset} onClick={() => void seek(1)}><Icon name="chevronDown" size={14} className="rotate-270" /></button></div><span className="icon-status" aria-label="Volume controls unavailable"><Icon name="volume" size={16} /></span></div>;
}

function Property({ label, value }: { label: string; value: string }) { return <div className="property-row"><span>{label}</span><strong title={value}>{value}</strong></div>; }

function Timeline({ snapshot, tracks, dispatch }: { snapshot: EditorSnapshot; tracks: ProjectTrack[]; dispatch: (command: EditorCommand) => Promise<EditorCommandResult> }) {
  const projection = useUiProjection();
  const timebase = snapshot.project?.sequence.timebase ?? null;
  const visibleTracks = tracks.length ? tracks : [{ id: "video-empty", kind: "Video" as const, name: "Video", enabled: true, locked: false, clips: [] }, { id: "audio-empty", kind: "Audio" as const, name: "Audio", enabled: true, locked: false, clips: [] }, { id: "subtitle-empty", kind: "Subtitle" as const, name: "Subtitles", enabled: true, locked: false, clips: [] }];
  const duration = tracks.flatMap((track) => track.clips).reduce((end, clip) => Math.max(end, clip.timeline_start + clip.timeline_duration), 0);
  const selectedClip = tracks.flatMap((track) => track.clips).find((clip) => clip.id === projection.selectedClipId) ?? null;
  const handleTimelineKeyDown = async (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === " ") {
      event.preventDefault();
      const result = await dispatch({ type: projection.isPlaying ? "previewPause" : "previewPlay" });
      if (result.status === "accepted") uiProjection.patch({ isPlaying: !projection.isPlaying });
      return;
    }
    if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
      event.preventDefault();
      const next = Math.max(0, projection.playheadTicks + (event.key === "ArrowRight" ? 1 : -1));
      const result = await dispatch({ type: "previewSeek", timelineTicks: next });
      if (result.status === "accepted") uiProjection.patch({ playheadTicks: next, announcement: `Playhead at ${formatTicks(next, timebase)}.` });
      return;
    }
    if (event.key === "Delete") {
      event.preventDefault();
      if (!selectedClip || !snapshot.project) {
        uiProjection.patch({ announcement: "BLOCKED — select a real clip before deleting." });
        return;
      }
      await dispatch({ type: "timelineApply", baseRevision: snapshot.project.revision, operation: { DeleteClip: { clip_id: selectedClip.id } } });
      return;
    }
    if (event.key.toLowerCase() === "s" && !event.ctrlKey && !event.altKey && !event.metaKey) {
      event.preventDefault();
      if (!selectedClip || !snapshot.project) {
        uiProjection.patch({ announcement: "BLOCKED — select a real clip before splitting." });
        return;
      }
      const splitAt = projection.playheadTicks;
      const clipEnd = selectedClip.timeline_start + selectedClip.timeline_duration;
      if (splitAt <= selectedClip.timeline_start || splitAt >= clipEnd) {
        uiProjection.patch({ announcement: "BLOCKED — move playhead inside selected clip before splitting." });
        return;
      }
      await dispatch({ type: "timelineApply", baseRevision: snapshot.project.revision, operation: { SplitClip: { clip_id: selectedClip.id, at_timeline_tick: splitAt } } });
    }
  };
  const markers = snapshot.project?.sequence.markers ?? [];
  return <section className="panel timeline-panel"><div className="timeline-header"><SectionHeading icon="scissors" eyebrow="SEQUENCE" title="Timeline" action={<div className="timeline-tools"><span className="zoom-label">100%</span></div>} /></div><div className="marker-strip"><span className="marker-label"><Icon name="marker" size={13} /> MARKERS</span>{markers.length ? <div className="marker-list">{markers.map((marker) => <span key={marker.id} className="marker-item">{marker.name} · {formatTicks(marker.position_ticks, timebase)}</span>)}</div> : <div className="marker-empty">No markers yet <span>Markers will appear after a host timeline operation.</span></div>}</div><div className="timeline-scroll" tabIndex={0} onKeyDown={(event) => void handleTimelineKeyDown(event)} role="region" aria-label="Timeline. Select a clip, use left and right arrows to seek, Delete to delete, S to split, and Space to play or pause."><div className="timeline-ruler"><div className="track-label-spacer" />{[0, 10, 20, 30, 40].map((seconds) => <span key={seconds}>{formatTicks(Math.round(seconds * (timebase?.numerator ?? 0) / (timebase?.denominator || 1)), timebase)}</span>)}</div>{visibleTracks.map((track) => <TrackLane key={track.id} track={track} timebase={timebase} selectedClipId={projection.selectedClipId} onSelect={(clip) => uiProjection.patch({ selectedClipId: clip.id, selectedAssetId: clip.asset_id, announcement: `Selected clip ${clip.id}.` })} />)}{!snapshot.project && <div className="timeline-empty-note"><Icon name="layout" size={16} /> Timeline is ready for a real project document.</div>}</div><div className="timeline-footer"><span><kbd>←</kbd><kbd>→</kbd> Seek</span><span><kbd>Space</kbd> Play / pause</span><span><kbd>Delete</kbd> Delete</span><span><kbd>S</kbd> Split</span><span className="timeline-footer-right">{duration ? formatTicks(duration, timebase) : "No duration available"}</span></div></section>;
}

function TrackLane({ track, timebase, selectedClipId, onSelect }: { track: ProjectTrack; timebase: Rational | null; selectedClipId: string | null; onSelect: (clip: ProjectClip) => void }) { return <div className="track-lane"><div className="track-label"><Icon name={track.kind === "Audio" ? "trackAudio" : track.kind === "Video" ? "trackVideo" : "message"} size={15} /><span>{track.name}</span><span className="track-lock" aria-label={track.locked ? "Track locked" : "Track unlocked"}><Icon name={track.locked ? "lock" : "check"} size={12} /></span></div><div className="track-canvas">{track.clips.length === 0 ? <span className="lane-empty">No clips</span> : track.clips.map((clip) => <button className={`timeline-clip ${selectedClipId === clip.id ? "is-selected" : ""}`} key={clip.id} type="button" aria-pressed={selectedClipId === clip.id} aria-label={`Select clip ${clip.id}, timeline duration ${formatTicks(clip.timeline_duration, timebase)}`} onClick={() => onSelect(clip)}>{clip.id}</button>)}</div></div>; }

function Assistant({ snapshot, projection, dispatch }: { snapshot: EditorSnapshot; projection: ReturnType<typeof useUiProjection>; dispatch: (command: EditorCommand) => Promise<EditorCommandResult> }) {
  const [draft, setDraft] = useState(projection.assistantDraft);
  const assistant = snapshot.capabilities.assistant;
  const submit = (event: FormEvent) => { event.preventDefault(); if (draft.trim()) void dispatch({ type: "assistantPlan", baseRevision: snapshot.project?.revision ?? 0, text: draft.trim() }); };
  const unavailable = assistant.state === "UNAVAILABLE";
  return <section className="panel assistant-panel"><SectionHeading icon="spark" eyebrow="ASSISTANT" title="Edit assistant" action={<StatusPill state={assistant.state}>{assistant.state}</StatusPill>} /><div className="assistant-unavailable"><div className="assistant-icon"><Icon name="spark" size={19} /></div><div><strong>{unavailable ? "Local plan generation is unavailable" : "Local plan generation is ready"}</strong><p>{assistant.reason} Prompts stay local and no plan will be fabricated.</p></div></div><form className="assistant-form" onSubmit={submit}><label htmlFor="assistant-prompt">Describe an edit</label><div className="assistant-input-wrap"><textarea id="assistant-prompt" value={draft} onChange={(event) => { setDraft(event.target.value); uiProjection.patch({ assistantDraft: event.target.value }); }} placeholder="e.g. split the selected clip at 00:10" rows={3} /><button className="send-button" type="submit" aria-label="Request edit plan" disabled={!draft.trim() || assistant.state !== "READY"}><Icon name="send" size={15} /></button></div><small>Requires a verified local model and a connected canonical project.</small></form></section>;
}

function Jobs({ snapshot, dispatch }: { snapshot: EditorSnapshot; dispatch: (command: EditorCommand) => Promise<EditorCommandResult> }) { return <section className="panel jobs-panel"><SectionHeading icon="activity" eyebrow="ACTIVITY" title="Jobs" action={<span className="job-count">{snapshot.jobs.length}</span>} />{snapshot.jobs.length ? snapshot.jobs.map((job) => <div className="job-row" key={job.id}><div className="job-heading"><span>{job.kind}</span><StatusPill state={job.state === "succeeded" ? "READY" : job.state === "cancelled" ? "UNAVAILABLE" : "BLOCKED"}>{job.state}</StatusPill></div><p>{job.error?.message ?? job.message}</p>{job.progress !== null && <progress value={job.progress} max={1} aria-label={`${job.kind} progress`} />}{job.state === "running" && <button className="button button-small" onClick={() => void dispatch({ type: "jobCancel", jobId: job.id })}>Cancel</button>}</div>) : <div className="jobs-empty"><Icon name="clock" size={16} /><span>No jobs reported.<small>Probe, preview, render, and export status will appear here.</small></span></div>}</section>; }

export default App;
