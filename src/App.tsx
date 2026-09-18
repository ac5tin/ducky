import { useEffect } from "react";
import { useStore } from "./store";
import { Sidebar } from "./components/Sidebar";
import { ChatView } from "./components/chat/ChatView";
import { ConnectorsView } from "./components/connectors/ConnectorsView";
import { SettingsView } from "./components/settings/SettingsView";
import { Onboarding } from "./components/Onboarding";
import { ElicitationModal } from "./components/modals/ElicitationModal";
import { SamplingModal } from "./components/modals/SamplingModal";
import { ApprovalModal } from "./components/modals/ApprovalModal";
import { UpdateModal } from "./components/modals/UpdateModal";
import { Toasts } from "./components/Toasts";
import { Icon } from "./components/icons";

export default function App() {
  const ready = useStore((s) => s.ready);
  const view = useStore((s) => s.view);
  const init = useStore((s) => s.init);

  useEffect(() => {
    init().catch((e) => console.error("init failed", e));
  }, [init]);

  if (!ready) {
    return (
      <div className="flex h-full items-center justify-center bg-slate-50 text-slate-400 dark:bg-slate-950 dark:text-slate-500">
        <Icon name="duck" className="mr-3 h-8 w-8 animate-pulse" />
        <span className="text-sm">Waddling up…</span>
      </div>
    );
  }

  return (
    <div className="flex h-full bg-white text-slate-800 dark:bg-slate-950 dark:text-slate-200">
      {view !== "onboarding" && <Sidebar />}
      <main className="min-w-0 flex-1">
        {view === "onboarding" && <Onboarding />}
        {view === "chat" && <ChatView />}
        {view === "connectors" && <ConnectorsView />}
        {view === "settings" && <SettingsView />}
      </main>

      {/* Global interactive modals (MCP client features) */}
      <ApprovalModal />
      <ElicitationModal />
      <SamplingModal />
      <UpdateModal />
      <Toasts />
    </div>
  );
}
