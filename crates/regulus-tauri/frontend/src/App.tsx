import { Activity, ArrowRight, Blocks, FolderOpen, GitBranch, Home, LayoutTemplate, PackageOpen, Settings, SlidersHorizontal } from "lucide-react";
import { lazy, Suspense, useEffect } from "react";
import Titlebar from "@/components/Titlebar";
import { Button } from "@/components/ui/button";
import { t, type LocaleKey } from "@/lib/i18n";
import { useCore, type CoreSource, type CoreStatus } from "@/core/connection";
import { moduleCountKey, useModuleList } from "@/core/modules";
import { useRegulusUi } from "@/store/useRegulusUi";
import SettingsScene from "@/settings/SettingsScene";
import "@/shell.css";

const CompositionScene = lazy(() => import("@/composition/CompositionScene"));
const OperateScene = lazy(() => import("@/operate/OperateScene"));
const TemplatesScene = lazy(() => import("@/templates/TemplatesScene"));

const navigation = [
  { id: "home", label: "shell.home", icon: Home },
  { id: "composition", label: "shell.composition", icon: GitBranch },
  { id: "operate", label: "operate.nav", icon: Activity },
  { id: "templates", label: "templates.nav", icon: LayoutTemplate },
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
      <section><div className="section-heading"><LayoutTemplate size={18} /><h2>{t("home.templates")}</h2></div><div className="empty-row"><h3>{t("templates.homeTitle")}</h3><p>{t("templates.homeDescription")}</p><Button variant="outline" size="sm" onClick={() => setView("templates")}>{t("templates.browse")}<ArrowRight size={14} /></Button></div></section>
    </div>
    <aside className="base-note"><Blocks size={22} /><div><h3>{t("home.base")}</h3><p>{t("home.baseDescription")}</p></div></aside>
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
  const modules = useModuleList();
  useEffect(() => { connect(); }, [connect]);
  return <div className="regulus-shell" data-density={density}>
    <Titlebar />
    <div className="shell-body">
      <aside className="shell-navigation"><div className="workspace-label">{t("shell.global")}</div><nav aria-label={t("app.name")}>{navigation.map(({ id, label, icon: Icon }) => <button key={id} aria-label={t(label)} onClick={() => setView(id)} aria-current={view === id ? "page" : undefined}><Icon size={18} strokeWidth={1.5} /><span>{t(label as LocaleKey)}</span></button>)}</nav><div className="sidebar-bottom"><PackageOpen size={16} /><span>{t(moduleCountKey(modules), { count: modules?.length ?? 0 })}</span></div></aside>
      <main className="shell-main">{view === "settings" ? <SettingsScene /> : view === "composition" ? <Suspense fallback={<div role="status" className="flex h-full items-center justify-center text-sm text-muted-foreground">{t("graph.loading")}</div>}><CompositionScene /></Suspense> : view === "operate" || view === "templates" ? <Suspense fallback={<div role="status" className="flex h-full items-center justify-center text-sm text-muted-foreground">{t("graph.loading")}</div>}>{view === "operate" ? <OperateScene /> : <TemplatesScene />}</Suspense> : <HomeScene />}</main>
    </div>
    <footer className="shell-status" role="status"><span>{t("shell.preview")}</span><span data-connection={status}>{t(connectionLabel(status, source))}</span></footer>
  </div>;
}
