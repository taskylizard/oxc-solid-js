/**
 * @oxc-solid-js/compiler - OXC-based JSX compiler for SolidJS
 *
 * CJS entry point - provides the same interface as babel-preset-solid.
 */

const { platform, arch } = require('node:process');
const { join } = require('node:path');

let nativeBinding = null;

const explicitPath = process.env.NAPI_RS_NATIVE_LIBRARY_PATH;
if (explicitPath) {
  try {
    nativeBinding = require(explicitPath);
  } catch (e) {
    console.warn(`@oxc-solid-js/compiler: Failed to load native module from ${explicitPath}.`);
    console.warn(e instanceof Error ? e.message : String(e));
  }
}

// Map Node.js platform/arch to binary file suffix
const platformMap = {
  'darwin-arm64': 'darwin-arm64',
  'darwin-x64': 'darwin-x64',
  'linux-x64': 'linux-x64-gnu',
  'linux-arm64': 'linux-arm64-gnu',
  'win32-x64': 'win32-x64-msvc',
  'win32-arm64': 'win32-arm64-msvc',
};

const platformKey = `${platform}-${arch}`;
const nativeTarget = platformMap[platformKey];

const optionalBinaryPackageMap = {
  'darwin-arm64': '@oxc-solid-js/compiler-darwin-arm64',
  'darwin-x64': '@oxc-solid-js/compiler-darwin-x64',
  'linux-x64': '@oxc-solid-js/compiler-linux-x64-gnu',
  'win32-x64': '@oxc-solid-js/compiler-win32-x64-msvc',
};

// Try to load the native module
if (!nativeBinding) {
  const loadErrors = [];

  try {
    if (nativeTarget) {
      // Try platform-specific binary colocated with package first
      nativeBinding = require(join(__dirname, `oxc-solid-js-compiler.${nativeTarget}.node`));
    } else {
      // Fallback to generic name
      nativeBinding = require(join(__dirname, 'oxc-solid-js-compiler.node'));
    }
  } catch (e) {
    loadErrors.push(e);
  }

  if (!nativeBinding) {
    const optionalPackage = optionalBinaryPackageMap[platformKey];

    if (optionalPackage) {
      try {
        nativeBinding = require(optionalPackage);
      } catch (e) {
        loadErrors.push(e);
      }
    }
  }

  if (!nativeBinding) {
    console.warn(
      `@oxc-solid-js/compiler: Native module not found for ${platformKey}. Run \`npm run build\` to compile.`,
    );

    for (const err of loadErrors) {
      console.warn(err instanceof Error ? err.message : String(err));
    }
  }
}

/**
 * Default options matching babel-preset-solid
 */
const defaultOptions = {
  moduleName: 'solid-js/web',
  builtIns: [
    'For',
    'Show',
    'Switch',
    'Match',
    'Suspense',
    'SuspenseList',
    'Portal',
    'Index',
    'Dynamic',
    'ErrorBoundary',
  ],
  contextToCustomElements: true,
  wrapConditionals: true,
  generate: 'dom', // 'dom' | 'ssr' | 'universal'
  hydratable: false,
  delegateEvents: true,
  sourceMap: false,
};

/**
 * Transform JSX source code
 * @param {string} source - The source code to transform
 * @param {object} options - Transform options
 * @returns {{ code: string, map?: string }}
 */
function transform(source, options = {}) {
  if (!nativeBinding) {
    throw new Error(
      '@oxc-solid-js/compiler: Native module not loaded. Ensure it is built for your platform.',
    );
  }

  const mergedOptions = { ...defaultOptions, ...options };

  // NAPI-RS automatically converts camelCase (JS) to snake_case (Rust)
  // so we can pass options directly without manual conversion
  return nativeBinding.transformJsx(source, mergedOptions);
}

/**
 * Create a preset configuration (for compatibility with babel-preset-solid interface)
 * @param {object} _context - Babel context (ignored, for compatibility)
 * @param {object} options - User options
 * @returns {object}
 */
function preset(_context, options = {}) {
  const mergedOptions = { ...defaultOptions, ...options };

  return {
    // Return the options that would be passed to the transform
    options: mergedOptions,

    // The transform function
    transform: (source) => transform(source, mergedOptions),
  };
}

/**
 * Low-level transform function from the native binding
 */
const transformJsx = nativeBinding ? nativeBinding.transformJsx : null;

exports.transform = transform;
exports.preset = preset;
exports.defaultOptions = defaultOptions;
exports.transformJsx = transformJsx;
exports.default = {
  transform,
  preset,
  defaultOptions,
  transformJsx,
};
