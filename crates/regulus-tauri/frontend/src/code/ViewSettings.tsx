// The view's settings (docs/04-client.md §11): each shows where its value comes from when
// config.toml sets it, and "reset" drops only what was changed here.
import { ArrowDown, ArrowUp, RotateCcw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { t, type LocaleKey } from "@/lib/i18n";
import { fill } from "@/operate/logic";
import { codePanelRegistry, moveCodePanel, toggleCodePanel, type CodePanelId } from "./panels";
import { codeFlags, codeLimits, codeTexts, useCodeSettings, type CodeFlagKey, type CodeNumberKey, type CodeSettingKey, type CodeTextKey } from "./settings";

export function ViewSettings() {
  const settings = useCodeSettings();
  const source = (key: CodeSettingKey) => settings.overrides[key] === undefined && settings.configuredFrom[key]
    ? <small className="operate-source">{t("operate.settings.fromFile", { file: settings.configuredFrom[key]! })}</small> : null;
  const title = (id: CodePanelId) => t(codePanelRegistry.find((panel) => panel.id === id)!.title);
  return <section className="operate-settings code-settings" aria-label={t("operate.settings.title")}>
    {(Object.keys(codeTexts) as CodeTextKey[]).map((key) => <label key={key} className="operate-field">
      <span>{t(`code.settings.${key}` as LocaleKey)}</span>
      <input value={settings[key]} maxLength={codeTexts[key].max} spellCheck={false} placeholder={key === "label" ? t("code.name") : codeTexts[key].default}
        onChange={(event) => settings.setSetting(key, event.target.value)} />
      <small>{t(`code.settings.${key}Description` as LocaleKey)}</small>{source(key)}
    </label>)}
    {(Object.keys(codeLimits) as CodeNumberKey[]).map((key) => <label key={key} className="operate-field">
      <span>{t(`code.settings.${key}` as LocaleKey)}</span>
      <input type="number" min={codeLimits[key].min} max={codeLimits[key].max} value={settings[key]} onChange={(event) => settings.setSetting(key, Number(event.target.value))} />
      <small>{t(`code.settings.${key}Description` as LocaleKey)}</small>{source(key)}
    </label>)}
    {(Object.keys(codeFlags) as CodeFlagKey[]).map((key) => <label key={key} className="operate-check">
      <input type="checkbox" checked={settings[key]} onChange={(event) => settings.setSetting(key, event.target.checked)} />{t(`code.settings.${key}` as LocaleKey)}{source(key)}
    </label>)}
    <fieldset className="operate-panels-layout">
      <legend>{t("code.settings.panels")}</legend>
      <small>{t("code.settings.panelsHelp")}</small>
      {!settings.ownPanels && settings.panelsFrom && <small className="operate-source">{t("operate.settings.fromFile", { file: settings.panelsFrom })}</small>}
      <ol>{settings.panels.map((entry, index) => <li key={entry.id}>
        <label className="operate-check"><input type="checkbox" checked={!entry.hidden} onChange={(event) => settings.setPanels(toggleCodePanel(settings.panels, entry.id, !event.target.checked))} />{title(entry.id)}</label>
        <Button variant="ghost" size="icon-sm" disabled={index === 0} aria-label={fill(t("operate.panels.up"), { panel: title(entry.id) })} onClick={() => settings.setPanels(moveCodePanel(settings.panels, entry.id, -1))}><ArrowUp size={13} /></Button>
        <Button variant="ghost" size="icon-sm" disabled={index === settings.panels.length - 1} aria-label={fill(t("operate.panels.down"), { panel: title(entry.id) })} onClick={() => settings.setPanels(moveCodePanel(settings.panels, entry.id, 1))}><ArrowDown size={13} /></Button>
      </li>)}</ol>
    </fieldset>
    <p className="code-fine">{t("code.settings.shared")}</p>
    <Button variant="outline" size="sm" onClick={settings.reset}><RotateCcw size={14} />{t("operate.settings.reset")}</Button>
  </section>;
}
