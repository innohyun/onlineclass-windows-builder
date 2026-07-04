import { defineConfig } from "vite";
import fs from "node:fs";

const packageJson = JSON.parse(fs.readFileSync(new URL("./package.json", import.meta.url), "utf8"));

export default defineConfig({
  clearScreen: false,
  define: {
    __APP_VERSION__: JSON.stringify(packageJson.version || "0.0.0"),
  },
  server: {
    port: 1440,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
});
