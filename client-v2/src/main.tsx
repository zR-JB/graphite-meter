// The composition root: one App, one console with every view in its place, and the page's tooltips.
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/primitives.css";
import "./ui/kit/kit.css";
import "./styles/overrides.css";
import { createApp } from "./app/app";
import { DetailsPanel } from "./ui/details/details";
import { HistoryWorkspace } from "./ui/history/history";
import { installTips } from "./ui/kit/tip";
import { ConnectivityIndicator, GaugePanel, PhaseToast, StatusBar } from "./ui/measure";
import { SettingsPanel } from "./ui/settings/settings";
import { mountConsole } from "./ui/shell/console";

const app = createApp();
installTips();
mountConsole(document.getElementById("app")!, app, {
  measurement: (shell) => <GaugePanel app={app} shell={shell} />,
  settings: (shell) => <SettingsPanel app={app} shell={shell} />,
  details: (shell) => <DetailsPanel app={app} shell={shell} />,
  history: (shell, selectedId) => <HistoryWorkspace app={app} shell={shell} selectedId={selectedId} />,
  status: (shell) => <StatusBar app={app} shell={shell} />,
  toast: (shell) => <PhaseToast app={app} shell={shell} />,
  connectivity: (shell) => <ConnectivityIndicator app={app} shell={shell} />,
});
app.boot();
