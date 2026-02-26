'use strict'

// Attempt to load the pre-built platform-specific native addon.
// In development / CI, fall back to the debug build in the Cargo target
// directory so that the package can be used before a release build is ready.

const { existsSync } = require('fs')
const { join } = require('path')

function loadNative() {
  // 1. Try the release build (npm publish artifact)
  const releasePath = join(__dirname, `gp2f_node.${process.platform}-${process.arch}.node`)
  if (existsSync(releasePath)) {
    return require(releasePath)
  }

  // 2. Try a generic .node file (placed by CI during packaging)
  const genericPath = join(__dirname, 'gp2f_node.node')
  if (existsSync(genericPath)) {
    return require(genericPath)
  }

  // 3. Development fallback: debug build from Cargo target directory
  const targetDebugPath = join(__dirname, '..', 'target', 'debug', 'gp2f_node.node')
  if (existsSync(targetDebugPath)) {
    return require(targetDebugPath)
  }

  // 4. Release fallback from Cargo target directory
  const targetReleasePath = join(__dirname, '..', 'target', 'release', 'gp2f_node.node')
  if (existsSync(targetReleasePath)) {
    return require(targetReleasePath)
  }

  throw new Error(
    '@gp2f/server: could not locate the native addon (gp2f_node.node).\n' +
    'Run `cargo build` inside the gp2f-node directory and try again.'
  )
}

const { p, PolicyBuilder, FieldBuilder, VibeBuilder } = require('./lib/policy-builder')

let native
try {
  native = loadNative()
} catch (_) {
  native = {}
}

module.exports = { ...native, p, PolicyBuilder, FieldBuilder, VibeBuilder }
