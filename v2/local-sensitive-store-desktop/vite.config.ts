import { defineConfig } from "vite";
import fs from "node:fs";
import { createRequire } from "node:module";

const packageJson = JSON.parse(fs.readFileSync(new URL("./package.json", import.meta.url), "utf8"));
const require = createRequire(import.meta.url);
const pdfAssets = ['pdf.min.mjs', 'pdf.worker.min.mjs'].map((name) => ({
  name, source: fs.readFileSync(require.resolve(`pdfjs-dist/build/${name}`)),
}));

export default defineConfig({
  clearScreen: false,
  resolve: {dedupe:Object.keys(packageJson.dependencies || {})},
  plugins: [{
    name: 'offline-document-pdf',
    generateBundle() {
      for (const asset of pdfAssets) this.emitFile({type:'asset',fileName:`work-note-vendor/${asset.name}`,source:asset.source});
    },
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const asset=pdfAssets.find((item) => request.url === `/work-note-vendor/${item.name}`);
        if(!asset) return next();
        response.setHeader('Content-Type','text/javascript');response.end(asset.source);
      });
    },
  }],
  define: {
    __APP_VERSION__: JSON.stringify(packageJson.version || "0.0.0"),
  },
  server: {
    port: 1440,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
});
