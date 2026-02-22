/** @type {import('tailwindcss').Config} */
export default {
  content: ["./src/**/*.{astro,html,js,ts}"],
  theme: {
    extend: {
      colors: {
        background: "hsl(60, 22%, 7%)",
        primary: "hsl(60, 4%, 95%)",
        secondary: "hsl(0, 0%, 55%)",
        decorative: "hsl(355, 7%, 45%)",
        fill: "hsl(0, 0%, 13%)",
      },
      fontFamily: {
        sans: ["Inter", "system-ui", "sans-serif"],
        mono: ["JetBrains Mono", "monospace"],
        serif: ["Noto Serif", "serif"],
      },
      gridTemplateColumns: {
        layout: "1fr min(720px, 100%) 1fr",
      },
    },
  },
  plugins: [],
};
