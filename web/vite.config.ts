import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

const backend = "http://127.0.0.1:8770";
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    strictPort: true,
    proxy: Object.fromEntries(
      ["/api", "/startup-", "/assets/voice"].map((path) => [
        path,
        {
          target: backend,
          changeOrigin: true,
          // The development server is the same-origin browser facade; preserve the
          // backend's exact Origin check instead of weakening its production policy.
          configure(proxy) {
            proxy.on("proxyReq", (req) => {
              if (req.getHeader("origin")) req.setHeader("origin", backend);
            });
          },
        },
      ]),
    ),
  },
  test: {
    environment: "jsdom",
    clearMocks: true,
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
