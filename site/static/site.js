(function () {
  var reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  // Agent tabs over the hero video.
  document.querySelectorAll("[data-tabs]").forEach(function (box) {
    var video = box.querySelector("video");
    var caption = box.querySelector("figcaption");
    box.querySelectorAll("[role=tab]").forEach(function (tab) {
      tab.addEventListener("click", function () {
        box.querySelectorAll("[role=tab]").forEach(function (t) { t.setAttribute("aria-selected", t === tab); });
        video.poster = tab.dataset.poster;
        video.src = tab.dataset.src;
        caption.textContent = tab.dataset.caption;
        if (!reduce) video.play().catch(function () {});
      });
    });
  });

  // Install tabs.
  document.querySelectorAll("[data-tabs-install]").forEach(function (box) {
    var tabs = box.querySelectorAll("[role=tab]");
    tabs.forEach(function (tab) {
      tab.addEventListener("click", function () {
        tabs.forEach(function (t) { t.setAttribute("aria-selected", t === tab); });
        box.querySelectorAll(".pane").forEach(function (p) { p.hidden = p.dataset.pane !== tab.dataset.pane; });
      });
    });
  });

  // Videos play only while on screen; with reduced motion they show controls instead.
  var videos = document.querySelectorAll("video");
  if (reduce) {
    videos.forEach(function (v) { v.removeAttribute("autoplay"); v.pause(); v.controls = true; });
  } else if ("IntersectionObserver" in window) {
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (e.isIntersecting) { e.target.play().catch(function () {}); } else { e.target.pause(); }
      });
    }, { threshold: 0.35 });
    videos.forEach(function (v) { io.observe(v); });
  }

  // Star count from the GitHub API, shown from 20 up. The button works without it.
  var gh = document.querySelector("[data-stars]");
  if (gh && window.fetch) {
    fetch("https://api.github.com/repos/GregoryBolshakov/peekme")
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (d) {
        if (d && typeof d.stargazers_count === "number" && d.stargazers_count >= 20) {
          var c = gh.querySelector(".count");
          c.textContent = d.stargazers_count;
          c.hidden = false;
        }
      })
      .catch(function () {});
  }
})();
