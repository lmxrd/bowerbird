// Copies the desktop app's bundles (`mise run build:desktop`) out of the
// target directory, wherever cargo puts it (cargo has no stable flag to put
// an artifact elsewhere), into dist/desktop/. Node, not a shell script, so
// that it runs on Windows too.
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { join } from "node:path";

const metadata = JSON.parse(
  execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps"], {
    encoding: "utf8",
  }),
);
const bundles = join(metadata.target_directory, "release", "bundle");
if (!existsSync(bundles)) {
  console.error(`No bundles in ${bundles}: did \`tauri build\` run?`);
  process.exit(1);
}
const out = join("dist", "desktop");
rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
// One folder per format (macos, dmg, msi, nsis, deb, rpm, appimage); the
// bundles themselves are what is in them.
const wanted = /\.(app|dmg|msi|exe|deb|rpm|AppImage)$/;
for (const format of readdirSync(bundles)) {
  for (const name of readdirSync(join(bundles, format))) {
    // rw.<pid>.*.dmg: the writable image of a .dmg build that failed.
    if (!wanted.test(name) || name.startsWith("rw.")) continue;
    cpSync(join(bundles, format, name), join(out, name), { recursive: true });
    console.log(`dist/desktop/${name}`);
  }
}
