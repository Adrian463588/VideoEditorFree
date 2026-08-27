import fs from "node:fs";
import { URL } from "node:url";

const path = new URL("../../resources/runtime/bundle-manifest.json", import.meta.url);
const manifest = JSON.parse(fs.readFileSync(path, "utf8"));
if (manifest.schema_version !== 1 || manifest.bundle_id !== "videoeditorfree-windows-x64") {
  throw new Error("unsupported bundle manifest identity");
}
if (manifest.target?.os !== "windows" || manifest.target?.architecture !== "x86_64") {
  throw new Error("bundle target is not Windows x64");
}
if (!Array.isArray(manifest.artifacts) || manifest.artifacts.length < 1) {
  throw new Error("bundle has no artifacts");
}
const ids = new Set();
for (const artifact of manifest.artifacts) {
  for (const field of ["id", "kind", "profile", "destination", "url", "source", "version", "license", "size_bytes", "sha256"]) {
    if (artifact[field] === undefined || artifact[field] === "") throw new Error(`${artifact.id ?? "artifact"} is missing ${field}`);
  }
  if (ids.has(artifact.id)) throw new Error(`duplicate artifact id: ${artifact.id}`);
  ids.add(artifact.id);
  const url = new URL(artifact.url);
  if (url.protocol !== "https:" || !["github.com", "huggingface.co"].includes(url.hostname)) {
    throw new Error(`${artifact.id} uses a non-allowlisted URL`);
  }
  if (!Number.isSafeInteger(artifact.size_bytes) || artifact.size_bytes <= 0) throw new Error(`${artifact.id} has invalid size`);
  if (!/^[a-f0-9]{64}$/i.test(artifact.sha256)) throw new Error(`${artifact.id} has no verified SHA-256`);
  if (artifact.destination.startsWith("/") || artifact.destination.includes("..")) throw new Error(`${artifact.id} has unsafe destination`);
}
console.log(`bundle manifest valid: ${manifest.artifacts.length} artifacts`);
