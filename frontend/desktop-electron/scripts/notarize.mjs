#!/usr/bin/env node
// electron-builder afterSign hook. No-op when APPLE_ID is unset — the
// signing/notarize codepath is wired but credentials are optional, so
// any contributor can build locally and CI signs only when secrets are
// configured.

import { notarize } from "@electron/notarize";

export default async function afterSign(context) {
  const { electronPlatformName, appOutDir, packager } = context;
  if (electronPlatformName !== "darwin") return;

  const appleId = process.env.APPLE_ID;
  const password = process.env.APPLE_APP_SPECIFIC_PASSWORD;
  const teamId = process.env.APPLE_TEAM_ID;

  if (!appleId || !password || !teamId) {
    console.log("notarize: skipped (APPLE_ID / APPLE_APP_SPECIFIC_PASSWORD / APPLE_TEAM_ID not set)");
    return;
  }

  const appName = packager.appInfo.productFilename;
  const appPath = `${appOutDir}/${appName}.app`;

  console.log(`notarize: submitting ${appPath} via notarytool…`);
  await notarize({
    appPath,
    appleId,
    appleIdPassword: password,
    teamId,
    tool: "notarytool",
  });
  console.log("notarize: done.");
}
