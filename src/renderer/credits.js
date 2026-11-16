import { invoke } from "@tauri-apps/api/core";

async function credits() {
  document.querySelector("#license").onclick = (event) => {
    event.preventDefault();
    invoke("open_license").catch((error) => console.error(error));
  };

  const info = await invoke("app_info");
  document.querySelector("#versions").innerHTML =
    `macintosh.js v${info.version} with Tauri`;
}

credits();
