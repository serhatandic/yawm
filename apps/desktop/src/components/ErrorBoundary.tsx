import { Component, type ErrorInfo, type ReactNode } from "react";
import { Button } from "@/components/ui/button";

/**
 * Keeps one broken render from taking the whole window with it.
 *
 * React unmounts the entire tree when a render throws and nothing catches it,
 * so a single undefined field blanked the app to an empty black rectangle with
 * no error, no list, and nothing to click. That is the worst shape a failure
 * can take here: this app is trusted to say what is safe to delete, and an
 * app that vanishes teaches the user that it is unreliable in general rather
 * than broken in one specific place.
 *
 * The message says what happened rather than apologising, and reloading is
 * offered because the state that caused it was fetched and will usually be
 * fetched again cleanly.
 */
export class ErrorBoundary extends Component<
  { children: ReactNode },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // Kept for the dev console and any log the window is piped to; there is
    // nowhere else for this to go in a desktop app with no reporting.
    console.error("yawm: render failed", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="flex h-full flex-col items-center justify-center gap-4 bg-background p-8 text-center">
        <div className="max-w-md space-y-2">
          <p className="text-sm font-medium">yawm stopped drawing this screen</p>
          <p className="text-xs text-muted-foreground">
            Something it was shown did not have the shape it expected. Nothing
            was changed on disk — this is a display failure, not an operation.
          </p>
          <p className="rounded-sm border border-border bg-card px-2 py-1.5 text-left font-mono text-[10px] break-all text-muted-foreground">
            {error.message}
          </p>
        </div>
        <Button size="sm" onClick={() => window.location.reload()}>
          Reload
        </Button>
      </div>
    );
  }
}
