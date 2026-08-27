import type { ReactNode, SVGProps } from "react";

export type IconName =
  | "activity"
  | "arrowDown"
  | "check"
  | "chevronDown"
  | "circleHelp"
  | "clock"
  | "download"
  | "film"
  | "folder"
  | "layout"
  | "lock"
  | "marker"
  | "message"
  | "more"
  | "pause"
  | "play"
  | "plus"
  | "save"
  | "scissors"
  | "send"
  | "settings"
  | "spark"
  | "trackAudio"
  | "trackVideo"
  | "undo"
  | "upload"
  | "volume";

const paths: Record<IconName, ReactNode> = {
  activity: <><path d="M3 12h4l2-7 4 14 2-7h6" /></>,
  arrowDown: <><path d="M12 4v15" /><path d="m6 13 6 6 6-6" /></>,
  check: <path d="m5 12 4 4L19 6" />,
  chevronDown: <path d="m6 9 6 6 6-6" />,
  circleHelp: <><circle cx="12" cy="12" r="9" /><path d="M9.7 9a2.35 2.35 0 1 1 3.8 1.85c-.95.7-1.5 1.05-1.5 2.4" /><path d="M12 16.5h.01" /></>,
  clock: <><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></>,
  download: <><path d="M12 4v15" /><path d="m6 13 6 6 6-6" /></>,
  film: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M7 4v16M17 4v16M3 9h4M17 9h4M3 15h4M17 15h4" /></>,
  folder: <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h5l2 2H19A1.5 1.5 0 0 1 20.5 8.5v9A1.5 1.5 0 0 1 19 19H4.5A1.5 1.5 0 0 1 3 17.5Z" />,
  layout: <><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M9 4v16M9 10h12" /></>,
  lock: <><rect x="5" y="10" width="14" height="10" rx="2" /><path d="M8 10V7a4 4 0 0 1 8 0v3" /></>,
  marker: <path d="m7 3 10 4-4 2 2 6-2 .7-2-6-3 2Z" />,
  message: <><path d="M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v8a2.5 2.5 0 0 1-2.5 2.5H11l-4.5 4v-4h0A2.5 2.5 0 0 1 4 13.5Z" /><path d="M8 8h8M8 11h5" /></>,
  more: <><circle cx="5" cy="12" r="1" fill="currentColor" stroke="none" /><circle cx="12" cy="12" r="1" fill="currentColor" stroke="none" /><circle cx="19" cy="12" r="1" fill="currentColor" stroke="none" /></>,
  pause: <><path d="M8 5v14M16 5v14" /></>,
  play: <path d="m8 5 11 7-11 7Z" />,
  plus: <><path d="M12 5v14M5 12h14" /></>,
  save: <><path d="M5 3h12l3 3v15H4V3Z" /><path d="M8 3v6h8V3M8 21v-7h8v7" /></>,
  scissors: <><circle cx="6" cy="7" r="2" /><circle cx="6" cy="17" r="2" /><path d="m8 8 10 8M8 16 18 8" /></>,
  send: <><path d="m3 4 18 8-18 8 3.5-8Z" /><path d="M6.5 12H21" /></>,
  settings: <><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-1.8 1.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5v.1h-2.5v-.1a1.7 1.7 0 0 0-1-1.5 1.7 1.7 0 0 0-1.9.3l-.1.1-1.8-1.8.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.5-1H6.5v-2.5h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.9l-.1-.1 1.8-1.8.1.1a1.7 1.7 0 0 0 1.9.3 1.7 1.7 0 0 0 1-1.5V4h2.5v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1 1.8 1.8-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.5 1h.1v2.5h-.1a1.7 1.7 0 0 0-1.5 1Z" /></>,
  spark: <path d="m12 3 1.6 5.4L19 10l-5.4 1.6L12 17l-1.6-5.4L5 10l5.4-1.6Z" />,
  trackAudio: <><path d="M4 16a2 2 0 1 0 4 0V8H5a1 1 0 0 0-1 1v7M8 8h10V5H8" /><path d="M18 5v11a2 2 0 1 0 4 0V9" /></>,
  trackVideo: <><rect x="3" y="6" width="13" height="12" rx="2" /><path d="m16 10 5-3v10l-5-3" /></>,
  undo: <><path d="M9 7 4 12l5 5" /><path d="M5 12h8a7 7 0 0 1 7 7" /></>,
  upload: <><path d="M12 16V4M7 9l5-5 5 5" /><path d="M5 20h14" /></>,
  volume: <><path d="M4 10v4h4l5 4V6L8 10Z" /><path d="M16 9a4 4 0 0 1 0 6M18.5 6.5a7.5 7.5 0 0 1 0 11" /></>,
};

export const Icon = ({ name, size = 16, ...props }: { name: IconName; size?: number } & SVGProps<SVGSVGElement>) => (
  <svg aria-hidden="true" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" {...props}>
    {paths[name]}
  </svg>
);
