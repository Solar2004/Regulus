import Titlebar from "@/components/Titlebar";

export default function App() {
  return (
    <div className="flex h-screen flex-col overflow-hidden bg-background text-foreground">
      <Titlebar />
      <main className="min-h-0 flex-1 bg-background" />
    </div>
  );
}
