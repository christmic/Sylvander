import { invoke } from "@tauri-apps/api/core";

export interface DesktopHostPreferences {
  turn_notifications: boolean;
}

export interface DesktopHostPort {
  getPreferences(): Promise<DesktopHostPreferences>;
  setTurnNotifications(enabled: boolean): Promise<DesktopHostPreferences>;
}

/** Narrow bridge for presentation-only native host preferences. */
export class DesktopHost implements DesktopHostPort {
  getPreferences() {
    return invoke<DesktopHostPreferences>("get_host_preferences");
  }

  setTurnNotifications(enabled: boolean) {
    return invoke<DesktopHostPreferences>("set_turn_notifications", { enabled });
  }
}
