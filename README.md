# oxc-solid-js

Native JSX compiler for [Solid.js](https://www.solidjs.com/) users built with the [OXC](https://oxc.rs/) toolchain.

Very work in progress, but I encourage you to try it out and report issues!

## Install

```sh
# compiler
npm install @oxc-solid-js/compiler

# vite plugin
npm install -D @oxc-solid-js/vite

# rolldown plugin
npm install -D @oxc-solid-js/rolldown
```

## Usage

### Vite

```js
// vite.config.js
import { defineConfig } from "vite";
import solidOxc from "@oxc-solid-js/vite";

export default defineConfig({
  plugins: [solidOxc()],
});
```

Options:

```js
solidOxc({
  generate: "dom", // "dom" | "ssr" | "universal"
  hydratable: false,
  hot: true, // HMR via solid-refresh (dev only)
});
```

### Rolldown

```js
// rolldown.config.js
import solidOxc from "@oxc-solid-js/rolldown";

export default {
  plugins: [solidOxc()],
};
```

### Direct API

```js
import { transformJsx } from "@oxc-solid-js/compiler";

const { code, map } = transformJsx(`<div class="hello">world</div>`, {
  generate: "dom",
  moduleName: "solid-js/web",
  sourceMap: true,
});
```

## License

This project is licensed under the [MIT License](./LICENSE).

This project also contains code derived or copied from the following projects:

- [@dom-expressions/babel-plugin-jsx (MIT)](https://github.com/ryansolid/dom-expressions/tree/next/packages/babel-plugin-jsx)
- [solid-refresh (MIT)](https://github.com/solidjs/solid-refresh)
- [vite-plugin-solid (MIT)](https://github.com/solidjs/vite-plugin-solid)

Licenses of these projects are listed in [THIRD-PARTY-LICENSES](./THIRD-PARTY-LICENSES).
