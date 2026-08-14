import { invoke } from "@tauri-apps/api/core";

import type { RuntimeUserProfileExport } from "./gateway";

export interface DesktopHostPreferences {
  turn_notifications: boolean;
}

export interface DesktopHostPort {
  getPreferences(): Promise<DesktopHostPreferences>;
  saveUserProfileExport(exported: RuntimeUserProfileExport): Promise<{ saved: boolean }>;
  setTurnNotifications(enabled: boolean): Promise<DesktopHostPreferences>;
}

/** Narrow bridge for presentation-only native host preferences. */
export class DesktopHost implements DesktopHostPort {
  getPreferences() {
    return invoke<DesktopHostPreferences>("get_host_preferences");
  }

  saveUserProfileExport(exported: RuntimeUserProfileExport) {
    return invoke<{ saved: boolean }>("save_user_profile_export", { export: exported });
  }

  setTurnNotifications(enabled: boolean) {
    return invoke<DesktopHostPreferences>("set_turn_notifications", { enabled });
  }
}
