/* Voice Button promo site — small, dependency-free enhancements.
   Everything on the page is readable without this file: it only adds the
   release fallback, copy-to-clipboard, reveal-on-scroll and the hero field. */
(() => {
  "use strict";

  const root = document.documentElement;
  root.classList.add("js");

  const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  const releasePage = "https://github.com/machine202502/voice-button/releases/latest";

  /* ---------------------------------------------------------- year */
  const year = new Date().getFullYear();
  document.querySelectorAll("[data-year]").forEach((node) => {
    node.textContent = String(year);
  });

  /* ------------------------------------- release placeholders fallback
     The Pages workflow fills the @@…@@ tokens with sed. If it had no release
     to read (or the tokens were left in place), keep the page honest: every
     download goes to the releases page and unknown values become a dash. */
  const tokenNodes = [
    ...document.querySelectorAll(
      "[data-version], [data-size-setup], [data-size-zip], [data-hash-setup], [data-hash-zip]"
    ),
  ];
  const downloadLinks = [
    ...document.querySelectorAll("[data-dl-setup], [data-dl-zip]"),
  ];
  const hasToken = (value) => (value || "").includes("@@");

  if (
    tokenNodes.some((node) => hasToken(node.textContent)) ||
    downloadLinks.some((link) => hasToken(link.getAttribute("href")))
  ) {
    downloadLinks.forEach((link) => {
      link.href = releasePage;
    });
    tokenNodes.forEach((node) => {
      if (hasToken(node.textContent)) node.textContent = "—";
    });
  }

  /* ------------------------------------------------- copy SHA-256 */
  document.querySelectorAll("[data-hash-setup], [data-hash-zip]").forEach((button) => {
    button.addEventListener("click", async () => {
      const text = button.textContent.trim();
      if (!text || hasToken(text)) return;
      try {
        await navigator.clipboard.writeText(text);
        button.classList.add("copied");
        window.setTimeout(() => button.classList.remove("copied"), 1200);
      } catch {
        const range = document.createRange();
        range.selectNodeContents(button);
        const selection = window.getSelection();
        if (!selection) return;
        selection.removeAllRanges();
        selection.addRange(range);
      }
    });
  });

  /* ------------------------------------------------------- reveal */
  const reveals = [...document.querySelectorAll("[data-reveal]")];
  if (!("IntersectionObserver" in window) || reduceMotion.matches) {
    reveals.forEach((node) => node.classList.add("is-in"));
  } else {
    const observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (!entry.isIntersecting) return;
          entry.target.classList.add("is-in");
          observer.unobserve(entry.target);
        });
      },
      { rootMargin: "0px 0px -8% 0px", threshold: 0.12 }
    );
    reveals.forEach((node) => observer.observe(node));
  }

  /* ---------------------------------------------------- hero field
     A slow topographic sound field behind the hero. It is drawn with the 2D
     canvas API, so no WebGL is involved; if the context or the API is missing
     the CSS gradient behind it is the whole visual. It stops while the hero is
     off-screen, while the tab is hidden, and never starts for reduced motion. */
  const canvas = document.querySelector("[data-field]");
  if (!canvas) return;
  const ctx = canvas.getContext("2d");
  if (!ctx) return;

  const colors = [
    [199, 243, 107],
    [185, 162, 240],
    [255, 189, 98],
  ];
  const layers = [
    { y: 0.42, amp: 24, freq: 0.0042, speed: 0.00018, color: 0, alpha: 0.34, width: 1.3 },
    { y: 0.52, amp: 32, freq: 0.0031, speed: -0.00014, color: 1, alpha: 0.26, width: 1.1 },
    { y: 0.63, amp: 18, freq: 0.0056, speed: 0.00022, color: 2, alpha: 0.16, width: 1 },
    { y: 0.72, amp: 40, freq: 0.0022, speed: -0.00009, color: 0, alpha: 0.12, width: 1 },
  ];

  let width = 0;
  let height = 0;
  let dpr = 1;
  let frame = 0;
  let inView = false;
  let started = false;

  const resize = () => {
    const rect = canvas.getBoundingClientRect();
    width = Math.max(1, Math.round(rect.width));
    height = Math.max(1, Math.round(rect.height));
    dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(height * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  };

  const draw = (time) => {
    ctx.clearRect(0, 0, width, height);
    const gradient = ctx.createLinearGradient(0, 0, width, 0);
    for (const layer of layers) {
      const [r, g, b] = colors[layer.color];
      gradient.addColorStop(0, `rgba(${r}, ${g}, ${b}, 0)`);
      gradient.addColorStop(0.22, `rgba(${r}, ${g}, ${b}, ${layer.alpha})`);
      gradient.addColorStop(0.72, `rgba(${r}, ${g}, ${b}, ${layer.alpha * 0.8})`);
      gradient.addColorStop(1, `rgba(${r}, ${g}, ${b}, 0)`);

      ctx.beginPath();
      ctx.strokeStyle = gradient;
      ctx.lineWidth = layer.width;
      const phase = layer.color * 1.7;
      for (let x = -24; x <= width + 24; x += 18) {
        const y =
          height * layer.y +
          Math.sin(x * layer.freq + time * layer.speed + phase) * layer.amp +
          Math.sin(x * layer.freq * 2.7 - time * layer.speed * 1.6 + phase) * layer.amp * 0.35;
        if (x === -24) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.stroke();
    }
  };

  const loop = (time) => {
    draw(time);
    frame = window.requestAnimationFrame(loop);
  };

  const stop = () => {
    if (frame) window.cancelAnimationFrame(frame);
    frame = 0;
  };

  const start = () => {
    if (frame || !inView || document.hidden || reduceMotion.matches) return;
    started = true;
    draw(performance.now());
    frame = window.requestAnimationFrame(loop);
  };

  const restart = () => {
    resize();
    if (!started || reduceMotion.matches) {
      if (!reduceMotion.matches) draw(performance.now());
      return;
    }
    stop();
    start();
  };

  const watchVisibility = () => {
    if (document.hidden) stop();
    else start();
  };

  if ("ResizeObserver" in window) {
    new ResizeObserver(restart).observe(canvas);
  } else {
    window.addEventListener("resize", restart);
  }

  if ("IntersectionObserver" in window) {
    const observer = new IntersectionObserver(
      (entries) => {
        inView = entries.some((entry) => entry.isIntersecting);
        if (inView) start();
        else stop();
      },
      { threshold: 0 }
    );
    observer.observe(canvas);
  } else {
    inView = true;
  }

  document.addEventListener("visibilitychange", watchVisibility);
  if (typeof reduceMotion.addEventListener === "function") {
    reduceMotion.addEventListener("change", (event) => {
      if (event.matches) stop();
      else start();
    });
  }

  resize();
  if (inView) start();
  else if (!reduceMotion.matches) draw(performance.now());
})();
