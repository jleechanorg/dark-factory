import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  experimental: {
    serverActions: {
      allowedOrigins: ["localhost:3000"],
    },
  },
  images: {
    domains: ["storage.googleapis.com", "localhost"],
  },
  async rewrites() {
    // Firebase emulator rewrites — only active when NEXT_PUBLIC_USE_EMULATORS=true
    if (process.env.NEXT_PUBLIC_USE_EMULATORS !== "true") return [];
    return [
      {
        source: "/__/firebase/:path*",
        destination: "http://localhost:5001/__/firebase/:path*",
      },
    ];
  },
};

export default nextConfig;
