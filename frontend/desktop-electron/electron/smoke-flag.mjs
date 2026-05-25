// Smoke-test flag: when present, skip onboarding, use the supplied
// vault dir, exit on first successful sidecar /health, and `app.quit`.

/**
 * @param {string[]} argv
 * @returns {{enabled: boolean, vault: string|null}}
 */
export function parseSmokeFlags(argv) {
  const enabled = argv.includes("--smoke-test");
  let vault = null;
  const idx = argv.indexOf("--vault");
  if (idx >= 0 && idx + 1 < argv.length) {
    vault = argv[idx + 1];
  }
  return { enabled, vault };
}
