document.querySelector("[data-year]").textContent = new Date().getFullYear();

const releasePage = "https://github.com/machine202502/voice-button/releases/latest";
const versionNodes = [...document.querySelectorAll("[data-version]")];
const downloadNodes = [...document.querySelectorAll("[data-dl-setup], [data-dl-zip]")];
const tokenNodes = [
  ...versionNodes,
  ...downloadNodes,
  ...document.querySelectorAll("[data-hash-setup], [data-hash-zip], [data-size-setup], [data-size-zip]")
];
const hasTokens = tokenNodes.some((node) =>
  node.textContent.includes("@@") || (node.getAttribute("href") || "").includes("@@")
);

function hideTokens() {
  for (const node of tokenNodes) {
    const link = node.hasAttribute("href");
    const filled = link
      ? !(node.getAttribute("href") || "").includes("@@")
      : !node.textContent.includes("@@");
    if (!filled && !link) node.style.display = "none";
  }
}

if (hasTokens) {
  downloadNodes.forEach((link) => { link.href = releasePage; });
  fetch("https://api.github.com/repos/machine202502/voice-button/releases/latest")
    .then((response) => response.ok ? response.json() : null)
    .then((release) => {
      if (!release) return;
      const version = (release.tag_name || "").replace(/^v/, "");
      const setup = release.assets.find((asset) => asset.name.endsWith("-setup.exe"));
      const zip = release.assets.find((asset) => asset.name.endsWith("windows-x64.zip"));
      if (version) {
        versionNodes.forEach((node) => { node.textContent = version; });
      }
      if (setup) {
        document.querySelectorAll("[data-dl-setup]").forEach((link) => { link.href = setup.browser_download_url; });
        document.querySelector("[data-size-setup]").textContent = `${(setup.size / 1048576).toFixed(1)} МБ`;
      }
      if (zip) {
        document.querySelectorAll("[data-dl-zip]").forEach((link) => { link.href = zip.browser_download_url; });
        document.querySelector("[data-size-zip]").textContent = `${(zip.size / 1048576).toFixed(1)} МБ`;
      }
    })
    .finally(hideTokens);
}

// Copy hashes on click.
document.querySelectorAll("[data-hash-setup], [data-hash-zip]").forEach((code) => {
  code.title = "SHA-256 — click to copy";
  code.addEventListener("click", async () => {
    const text = code.textContent.trim();
    if (!text || text.includes("@@")) return;
    try {
      await navigator.clipboard.writeText(text);
      code.classList.add("copied");
      setTimeout(() => code.classList.remove("copied"), 1200);
    } catch {
      // Clipboard unavailable: select the text instead.
      const range = document.createRange();
      range.selectNodeContents(code);
      const selection = getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
    }
  });
});

// Remember the language the visitor picked.
document.querySelectorAll("[data-lang-set]").forEach((link) => {
  link.addEventListener("click", () => {
    try {
      localStorage.setItem("voice-button-lang", link.dataset.langSet);
    } catch {
      // Private mode can reject storage; the link still opens the page.
    }
  });
});
