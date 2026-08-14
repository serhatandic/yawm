import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// Tauri drives this dev server, so the port is fixed and failure must be loud
// rather than silently falling back to another port the app is not pointed at.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    // `import.meta.dirname` rather than `__dirname`: this config is ESM, and
    // Vite 8's native config loader does not provide the CommonJS globals.
    alias: { "@": new URL("./src", import.meta.url).pathname },
  },
  clearScreen: false,
  server: {
    port: 5178,
    strictPort: true,
    watch: {
      // Rust rebuilds are handled by Tauri; watching target/ would thrash.
      ignored: ["**/src-tauri/**", "**/target/**"],
    },
  },
  build: {
    // Match the oldest engine among the three system webviews yawm targets.
    target: ["es2022", "safari15"],
    sourcemap: false,
  },
});
