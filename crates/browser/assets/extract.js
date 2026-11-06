// Runs in the loaded page right after Readability.js (Mozilla Readability 0.6.0, Apache-2.0,
// vendored verbatim from https://raw.githubusercontent.com/mozilla/readability/0.6.0/Readability.js).
// The completion value is a JSON string that engine.rs parses into `Extracted`.
(function () {
  function absolute(url) {
    try {
      return new URL(url, document.baseURI).href;
    } catch (e) {
      return null;
    }
  }

  // Every JSON-LD object on the page, flattening arrays and @graph lists.
  function jsonLdObjects() {
    var found = [];
    function walk(value) {
      if (Array.isArray(value)) {
        value.forEach(walk);
      } else if (value && typeof value === "object") {
        found.push(value);
        if (value["@graph"]) walk(value["@graph"]);
      }
    }
    document.querySelectorAll('script[type="application/ld+json"]').forEach(function (script) {
      try {
        walk(JSON.parse(script.textContent));
      } catch (e) {
        // Broken JSON-LD is common; ignore it.
      }
    });
    return found;
  }

  function jsonLdImage(objects) {
    for (var i = 0; i < objects.length; i++) {
      var image = objects[i].image;
      if (Array.isArray(image)) image = image[0];
      if (image && typeof image === "object") image = image.url;
      if (typeof image === "string" && image) return image;
    }
    return null;
  }

  function wideImage() {
    var images = document.images;
    for (var i = 0; i < images.length; i++) {
      var img = images[i];
      var width = img.naturalWidth || parseInt(img.getAttribute("width"), 10) || 0;
      if (width >= 400 && img.src) return img.src;
    }
    return null;
  }

  var objects = jsonLdObjects();
  var paywalled = objects.some(function (o) {
    var free = o.isAccessibleForFree;
    return free === false || (typeof free === "string" && free.toLowerCase() === "false");
  });

  var og = document.querySelector('meta[property="og:image"]');
  var picture = (og && og.getAttribute("content")) || jsonLdImage(objects) || wideImage();

  var article = null;
  try {
    article = new Readability(document.cloneNode(true)).parse();
  } catch (e) {
    article = null;
  }

  var status = null;
  try {
    var nav = performance.getEntriesByType("navigation")[0];
    if (nav && typeof nav.responseStatus === "number" && nav.responseStatus > 0) {
      status = nav.responseStatus;
    }
  } catch (e) {
    status = null;
  }

  var cookieNames = document.cookie
    .split(";")
    .map(function (pair) {
      return pair.split("=")[0].trim();
    })
    .filter(function (name) {
      return name.length > 0;
    });

  return JSON.stringify({
    main_status: status,
    title: (article && article.title) || document.title || "",
    text: (article && article.textContent) || "",
    html_lang: document.documentElement.lang || null,
    picture_url: picture ? absolute(picture) : null,
    paywalled: paywalled,
    cookie_names: cookieNames,
  });
})();
