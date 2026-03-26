/** Shared constants for the fbuild VSCode extension. */

/** Build profile labels (shown in quick-pick menus). */
export const BUILD_PROFILES: Record<string, string> = {
  quick: "Quick",
  release: "Release",
  debug: "Debug",
};

/** Build profile labels with icons (shown in quick-pick menus). */
export const BUILD_PROFILE_ICONS: Record<string, string> = {
  quick: "$(zap) Quick",
  release: "$(package) Release",
  debug: "$(bug) Debug",
};

/** Action labels (shown in quick-pick menus). */
export const ACTIONS: Record<string, string> = {
  "build+deploy+monitor": "Build + Deploy + Monitor",
  build: "Build Only",
  deploy: "Deploy Only",
};

/** Action labels with icons (shown in quick-pick menus). */
export const ACTION_ICONS: Record<string, string> = {
  "build+deploy+monitor": "$(rocket) Build + Deploy + Monitor",
  build: "$(tools) Build Only",
  deploy: "$(cloud-upload) Deploy Only",
};

/** Short action labels for the compact status bar summary. */
export const ACTION_SHORT: Record<string, string> = {
  "build+deploy+monitor": "B+D+M",
  build: "Build",
  deploy: "Deploy",
};

/** QuickPickItem with an attached value. */
export interface ValueQuickPickItem {
  label: string;
  description?: string;
  detail?: string;
  value: string;
}
