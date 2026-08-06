/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        ink: {
          950: '#0c0c0e',
          900: '#141417',
          800: '#1c1c21',
          700: '#26262d',
          600: '#34343d',
          500: '#4a4a56',
          400: '#6f6f7e',
          300: '#9a9aa8',
          200: '#c6c6d0',
        },
        accent: '#e0563f',
      },
    },
  },
  plugins: [],
}
