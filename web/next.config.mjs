/** @type {import('next').NextConfig} */
const nextConfig = {
  // Single-image deploy: emits .next/standalone (a minimal node server
  // the Rust gateway proxies to — see Dockerfile + API_INTERNAL_URL).
  output: "standalone",
  // Enables forbidden()/unauthorized() → forbidden.tsx / unauthorized.tsx
  // (real 403/401 pages instead of redirect-or-404 for auth failures).
  experimental: { authInterrupts: true },
  // Dev convenience: `npm run dev` on :3000 forwards /api/* to the Rust
  // server so the frontend is self-contained (client apiFetch calls stay
  // same-origin). In the single-image deploy axum intercepts /api/* before
  // the proxy, so this rewrite never fires there.
  async rewrites() {
    const api = process.env.API_INTERNAL_URL ?? "http://127.0.0.1:8080";
    return [{ source: "/api/:path*", destination: `${api}/api/:path*` }];
  },
};

export default nextConfig;
