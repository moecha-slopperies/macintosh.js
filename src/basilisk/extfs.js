// Host-folder sync for the emulator worker. Tauri webviews and workers do not
// have Node's synchronous filesystem API, so these commands are relayed to the
// main webview, where the Tauri host reads and writes ~/macintosh.js.

let nextRequest = 1;
const pending = new Map();

export function callHost(command, args) {
  const id = nextRequest++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    self.postMessage({ type: "host-invoke", id, command, args });
  });
}

self.addEventListener("message", (event) => {
  const message = event.data;
  if (message?.type !== "host-invoke-result") return;
  const request = pending.get(message.id);
  if (!request) return;
  pending.delete(message.id);
  if (message.error) request.reject(new Error(message.error));
  else request.resolve(message.result);
});

export async function readSharedTree(hostBase) {
  try {
    return await callHost("read_shared_tree", { hostBase });
  } catch (error) {
    console.error(`extfs preload: cannot read ${hostBase}`, error);
    return [];
  }
}

export function preloadExtfsTree(FS, entries, virtualBase) {
  const base = virtualBase.replace(/\/$/, "");
  for (const entry of entries) {
    const virtualPath = `${base}/${entry.name}`;
    const parentVirtual = virtualPath.split("/").slice(0, -1).join("/");
    const basename = entry.name.split("/").slice(-1)[0];
    if (entry.directory) {
      try {
        FS.mkdir(virtualPath);
      } catch {
        // already exists
      }
    } else {
      FS.createDataFile(
        parentVirtual,
        basename,
        new Uint8Array(entry.data),
        true,
        true,
        true,
      );
    }
  }
}

export async function sharedDirectory() {
  return callHost("get_shared_path");
}

export async function listUserImages(directory) {
  return callHost("list_user_images", { directory });
}

export function snapshotExtfsTree(FS, virtualBase) {
  const files = [];

  function visit(virtualDirectory, relativeDirectory) {
    const names = FS.readdir(virtualDirectory).filter(
      (name) => !name.startsWith("."),
    );
    for (const name of names) {
      const virtualPath = `${virtualDirectory}/${name}`;
      const relativePath = relativeDirectory
        ? `${relativeDirectory}/${name}`
        : name;
      const stat = FS.analyzePath(virtualPath);
      if (stat?.object?.isFolder) {
        visit(virtualPath, relativePath);
      } else if (stat?.object?.contents) {
        files.push({
          path: relativePath,
          data: new Uint8Array(stat.object.contents),
        });
      }
    }
  }

  visit(virtualBase, "");
  return files;
}
