import { useState } from "react";
import { t } from "@/lib/i18n";
import { useRegulusUi } from "@/store/useRegulusUi";
import "@/operate/operate.css";
import "./settings.css";
import { ModuleSettings } from "./ModuleSettings";
import { SecretSettings } from "./SecretSettings";
import { ServiceSettings } from "./ServiceSettings";
import { UpdateSettings } from "./UpdateSettings";

const sections = ["general", "modules", "secrets", "service", "presets"] as const;
type Section = (typeof sections)[number];
/** Sections whose values stay on this device; the others change the core. */
const local: Section[] = ["general", "presets"];

export default function SettingsScene() {
  const [section, setSection] = useState<Section>("modules");
  const { density, startPage, setDensity, setStartPage, applyPreset } = useRegulusUi();
  const preset = density === "compact" && startPage === "home" ? "compact" : density === "comfortable" ? startPage === "composition" ? "focus" : "default" : null;
  return <div className="scene settings-scene">
    <h1>{t("settings.title")}</h1><p className="scene-description">{t("settings.description")}</p>
    <div className="settings-layout">
      <nav aria-label={t("shell.settings")} className="settings-nav">{sections.map((id) => <button key={id} aria-current={section === id ? "page" : undefined} onClick={() => setSection(id)}>{t(`settings.${id}`)}</button>)}</nav>
      <section className="settings-content">
        {section === "modules" && <ModuleSettings />}
        {section === "secrets" && <SecretSettings />}
        {section === "service" && <ServiceSettings />}
        {section === "general" && <>
          <div className="settings-row"><div><h2>{t("settings.appearance")}</h2><p>{t("settings.appearanceDescription")}</p></div><select aria-label={t("settings.density")} value={density} onChange={(e) => setDensity(e.target.value as typeof density)}><option value="comfortable">{t("density.comfortable")}</option><option value="compact">{t("density.compact")}</option></select></div>
          <div className="settings-row"><div><h2>{t("settings.start")}</h2><p>{t("settings.startDescription")}</p></div><select aria-label={t("settings.start")} value={startPage} onChange={(e) => setStartPage(e.target.value as typeof startPage)}><option value="home">{t("shell.home")}</option><option value="composition">{t("shell.composition")}</option></select></div>
          <UpdateSettings />
        </>}
        {section === "presets" && <><p className="preset-description">{t("settings.presetsDescription")}</p><div className="presets">{(["default", "focus", "compact"] as const).map((id) => <button key={id} aria-pressed={preset === id} className="preset" onClick={() => applyPreset(id)}><div className={`preset-preview preset-preview-${id}`} aria-hidden="true"><i /><div><i /><i /><i /></div></div><h3>{t(`preset.${id}`)}</h3><p>{t(`preset.${id}Description`)}</p><span>{t(preset === id ? "preset.selected" : "preset.apply")}</span></button>)}</div></>}
        <p className="local-note">{t(local.includes(section) ? "settings.local" : "settings.core")}</p>
      </section>
    </div>
  </div>;
}
