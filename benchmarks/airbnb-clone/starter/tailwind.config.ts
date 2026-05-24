import type { Config } from "tailwindcss";

const config: Config = {
  content: [
    "./src/pages/**/*.{js,ts,jsx,tsx,mdx}",
    "./src/components/**/*.{js,ts,jsx,tsx,mdx}",
    "./src/app/**/*.{js,ts,jsx,tsx,mdx}",
  ],
  theme: {
    extend: {
      colors: {
        // Airbnb-style palette stubs — replace with brand values in Sprint 1
        brand: {
          50: "#fff1f0",
          100: "#ffe4e1",
          200: "#ffbdb6",
          300: "#ff8f84",
          400: "#ff5a5f",   // Airbnb Rausch (primary)
          500: "#e8484d",
          600: "#c9373c",
          700: "#a32c31",
          800: "#7f2227",
          900: "#5c1a1e",
        },
        surface: {
          DEFAULT: "#ffffff",
          muted: "#f7f7f7",
          border: "#dddddd",
        },
        text: {
          primary: "#222222",
          secondary: "#717171",
          tertiary: "#b0b0b0",
        },
      },
      fontFamily: {
        sans: ["var(--font-circular)", "Circular", "system-ui", "sans-serif"],
      },
      borderRadius: {
        DEFAULT: "0.75rem",
        card: "1.25rem",
      },
    },
  },
  plugins: [],
};

export default config;
