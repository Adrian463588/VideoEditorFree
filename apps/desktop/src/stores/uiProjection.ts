import { useSyncExternalStore } from "react";

export interface UiProjection {
  selectedAssetId: string | null;
  selectedClipId: string | null;
  playheadTicks: number;
  isPlaying: boolean;
  zoom: number;
  mediaFilter: "all" | "used";
  assistantDraft: string;
  announcement: string;
}

const initialProjection: UiProjection = {
  selectedAssetId: null,
  selectedClipId: null,
  playheadTicks: 0,
  isPlaying: false,
  zoom: 1,
  mediaFilter: "all",
  assistantDraft: "",
  announcement: "",
};

let projection = initialProjection;
const listeners = new Set<() => void>();

export const uiProjection = {
  getSnapshot: () => projection,
  subscribe: (listener: () => void) => {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  patch: (next: Partial<UiProjection>) => {
    const keys = Object.keys(next) as Array<keyof UiProjection>;
    if (keys.length === 0 || keys.every((key) => Object.is(projection[key], next[key]))) return;
    projection = { ...projection, ...next };
    listeners.forEach((listener) => listener());
  },
};

/** UI-only projection boundary. It must never become a second ProjectDocument store. */
export const useUiProjection = () => useSyncExternalStore(uiProjection.subscribe, uiProjection.getSnapshot, uiProjection.getSnapshot);
