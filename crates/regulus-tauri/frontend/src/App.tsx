import { ArrowRight, Blocks, FolderOpen, GitBranch, Home, LayoutTemplate, PackageOpen, Settings, SlidersHorizontal } from "lucide-react";
import { lazy, Suspense, useEffect, useState } from "react";
import Titlebar from "@/components/Titlebar";
import { Button } from "@/components/ui/button";
import { t, type LocaleKey } from "@/lib/i18n";
import { useCore, type CoreSource, type CoreStatus } from "@/core/connection";
import { useRegulusUi } from "@/store/useRegulusUi";
import "@/shell.css";

const CompositionScene = lazy(() => import("@/composition/CompositionScene"));

const navigation = [
  { id: "home", label: "shell.home", icon: Home },
  { id: "composition", label: "shell.composition", icon: GitBranch },
  { id: "settings", label: "shell.settings", icon: Settings },
] as const;

function HomeScene() {
  const setView = useRegulusUi((s) => s.setView);
  return <div className="scene home-scene">
    <div className="eyebrow">{t("home.eyebrow")}</div>
    <h1>{t("home.title")}</h1>
    <p className="scene-description">{t("home.description")}</p>
    <div className="home-actions">
      <Button onClick={() => setView("settings")}><SlidersHorizontal size={16} />{t("home.modules")}</Button>
      <Button variant="outline" onClick={() => setView("composition")}>{t("home.canvas")}<ArrowRight size={16} /></Button>
    </div>
    <div className="workspace-sections">
      <section><div className="section-heading"><FolderOpen size={18} /><h2>{t("home.projects")}</h2></div><div className="empty-row"><h3>{t("home.projectsEmpty")}</h3><p>{t("home.projectsDescription")}</p></div></section>
      <section><div className="section-heading"><LayoutTemplate size={18} /><h2>{t("home.templates")}</h2></div><div className="empty-row"><h3>{t("home.templatesEmpty")}</h3><p>{t("home.templatesDescription")}</p></div></section>
    </div>
    <aside className="base-note"><Blocks size={22} /><div><h3>{t("home.base")}</h3><p>{t("home.baseDescription")}</p></div></aside>
  </div>;
}

function ModuleSettings() {
  return <>
    <div className="settings-row"><div><h2>{t("settings.moduleSpace")}</h2><p>{t("settings.moduleSpaceDescription")}</p></div></div>
    <div className="module-empty"><PackageOpen size={36} strokeWidth={1.25} /><h3>{t("settings.modulesEmpty")}</h3><p>{t("settings.modulesDescription")}</p></div>
    <div className="layers"><h3>{t("settings.layers")}</h3><p>{t("settings.layersDescription")}</p></div>
  </>;
}

function SettingsScene() {
  const [section, setSection] = useState<"general" | "modules" | "presets">("modules");
  const { density, startPage, setDensity, setStartPage, applyPreset } = useRegulusUi();
  const preset = density === "compact" && startPage === "home" ? "compact" : density === "comfortable" ? startPage === "composition" ? "focus" : "default" : null;
  return <div className="scene settings-scene">
    <h1>{t("settings.title")}</h1><p className="scene-description">{t("settings.description")}</p>
    <div className="settings-layout">
      <nav aria-label={t("shell.settings")} className="settings-nav">{(["general", "modules", "presets"] as const).map((id) => <button key={id} aria-current={section === id ? "page" : undefined} onClick={() => setSection(id)}>{t(`settings.${id}`)}</button>)}</nav>
      <section className="settings-content">
        {section === "modules" && <ModuleSettings />}
        {section === "general" && <>
          <div className="settings-row"><div><h2>{t("settings.appearance")}</h2><p>{t("settings.appearanceDescription")}</p></div><select aria-label={t("settings.density")} value={density} onChange={(e) => setDensity(e.target.value as typeof density)}><option value="comfortable">{t("density.comfortable")}</option><option value="compact">{t("density.compact")}</option></select></div>
          <div className="settings-row"><div><h2>{t("settings.start")}</h2><p>{t("settings.startDescription")}</p></div><select aria-label={t("settings.start")} value={startPage} onChange={(e) => setStartPage(e.target.value as typeof startPage)}><option value="home">{t("shell.home")}</option><option value="composition">{t("shell.composition")}</option></select></div>
        </>}
        {section === "presets" && <><p className="preset-description">{t("settings.presetsDescription")}</p><div className="presets">{(["default", "focus", "compact"] as const).map((id) => <button key={id} aria-pressed={preset === id} className="preset" onClick={() => applyPreset(id)}><div className={`preset-preview preset-preview-${id}`} aria-hidden="true"><i /><div><i /><i /><i /></div></div><h3>{t(`preset.${id}`)}</h3><p>{t(`preset.${id}Description`)}</p><span>{t(preset === id ? "preset.selected" : "preset.apply")}</span></button>)}</div></>}
        <p className="local-note">{t("settings.local")}</p>
      </section>
    </div>
  </div>;
}

function connectionLabel(status: CoreStatus, source: CoreSource): LocaleKey {
  if (status === "loading") return "shell.connecting";
  if (status === "incompatible") return "graph.incompatible";
  if (status === "offline") return "shell.offline";
  return source === "mock" ? "graph.mock" : "shell.connected";
}

export default function App() {
  const { view, setView, density } = useRegulusUi();
  const { status, source, connect } = useCore();
  useEffect(() => { connect(); }, [connect]);
  return <div className="regulus-shell" data-density={density}>
    <Titlebar />
    <div className="shell-body">
      <aside className="shell-navigation"><div className="workspace-label">{t("shell.global")}</div><nav aria-label={t("app.name")}>{navigation.map(({ id, label, icon: Icon }) => <button key={id} aria-label={t(label)} onClick={() => setView(id)} aria-current={view === id ? "page" : undefined}><Icon size={18} strokeWidth={1.5} /><span>{t(label as LocaleKey)}</span></button>)}</nav><div className="sidebar-bottom"><PackageOpen size={16} /><span>{t("settings.modulesEmpty")}</span></div></aside>
      <main className="shell-main">{view === "settings" ? <SettingsScene /> : view === "composition" ? <Suspense fallback={<div role="status" className="flex h-full items-center justify-center text-sm text-muted-foreground">{t("graph.loading")}</div>}><CompositionScene /></Suspense> : <HomeScene />}</main>
    </div>
    <footer className="shell-status" role="status"><span>{t("shell.preview")}</span><span data-connection={status}>{t(connectionLabel(status, source))}</span></footer>
  </div>;
}
