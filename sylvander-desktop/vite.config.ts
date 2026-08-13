import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  plugins: [svelte()],
  server: {
    port: 1420,
    strictPort: true,
  },
});
