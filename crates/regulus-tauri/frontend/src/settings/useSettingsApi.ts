import { useMemo } from "react";
import { useCore } from "@/core/connection";
import { settingsApi, type SettingsApi } from "./client";

/** The Settings calls over the shared connection; `null` while there is no core to ask. */
export function useSettingsApi(): SettingsApi | null {
  const { client, status } = useCore();
  return useMemo(() => client && status === "ready" ? settingsApi(client) : null, [client, status]);
}
