# Rust Liquid

Liquid template rendering for Node.js, implemented in Rust and exposed through
Node-API. `LiquidEngine` supports the standard Liquid filters and tags, custom
JavaScript callbacks, partials, and layouts.

## Installation

Install the published package:

```sh
npm install @bagandev/liquid
```

The package loads a native Node-API binding for the current platform. To build
from this repository, install the Node.js dependencies and Rust toolchain, then
run:

```sh
npm install
npm run build
```

## Quick start

```js
import { LiquidEngine } from "@bagandev/liquid";

const engine = new LiquidEngine();
const html = await engine.render(
  "Hello {{ name | upcase }}!",
  { name: "Ada" },
);

console.log(html); // Hello ADA!
```

`render(template, context)` returns a promise that resolves to the rendered
string. The context should be a JSON-compatible object.

## Custom filters, tags, and blocks

Register JavaScript callbacks to add template behavior. Filter callbacks
receive the input and positional arguments. Tag callbacks receive evaluated
arguments, while block callbacks receive the rendered block body and evaluated
arguments.

```js
const engine = new LiquidEngine()
  .registerFilter("currency", async (amount, [symbol = "$"]) =>
    `${symbol}${Number(amount).toFixed(2)}`,
  )
  .registerTag("badge", ([label]) => `<strong>${label}</strong>`)
  .registerBlock("panel", (body, [title = "Info"]) =>
    `<section><h2>${title}</h2>${body.trim()}</section>`,
  );

const html = await engine.render(
  '{% badge "New" %}{{ price | currency: "€" }} {% panel "Details" %}Item{% endpanel %}',
  { price: 12.5 },
);
```

Filters may return a value asynchronously. Tags and blocks may return a string
or a promise resolving to one. Methods return the engine, so registrations can
be chained.

## Partials

Partials can be loaded from a directory, a JavaScript callback, or both. The
default extension search order is `.liquid`, then `.html`.

```js
const engine = new LiquidEngine()
  .setPartialsDir("./partials")
  .registerPartialLoader(async (name) => {
    if (name === "navigation") return "<nav>{{ title }}</nav>";
    return undefined;
  });

const html = await engine.render(
  '{% include "navigation" %}',
  { title: "Home" },
);
```

When both a loader and directory are configured, `LoaderFirst` is the default.
Set the priority explicitly with `.setPartialPriority("FileSystemFirst")` or
`.setPartialPriority("LoaderFirst")`. Use `.setPartialExtensions(["html",
"liquid"])` to change extension lookup order. A custom loader should resolve to
the template source, or to `undefined` when it does not have that partial.

## Layouts and front matter

Templates can choose a layout with YAML front matter or a leading Liquid
`layout` tag. Layouts are loaded from a directory or a JavaScript callback.
YAML keys (other than `layout`) are available under `page`; layout front matter
is available under `layout`. The rendered inner template is available to the
layout as `content` and `content_for_layout`.

```js
const engine = new LiquidEngine().registerLayoutLoader(async (name) => {
  if (name === "base") return "<main>{{ content }}</main>";
  return undefined;
});

const html = await engine.render(
  "---\nlayout: base\ntitle: Welcome\n---\n<h1>{{ page.title }}</h1>",
  {},
);
// <main><h1>Welcome</h1></main>
```

You can also configure filesystem layouts with `.setLayoutsDir("./layouts")`.
Layout extensions default to `.liquid`, then `.html`; configure their lookup
order with `.setLayoutExtensions(["html", "liquid"])`. Use
`.setLayoutPriority("FileSystemFirst")` or `.setLayoutPriority("LoaderFirst")`
when both a directory and loader are configured.

## Plugins

Use a plugin function to group engine configuration:

```js
const commonFilters = (engine) => {
  engine.registerFilter("shout", async (value) => `${String(value).toUpperCase()}!`);
};

const engine = new LiquidEngine().use(commonFilters);
```

## Front matter utilities

`frontMatter(source)` parses YAML front matter and returns `{ layout, body,
frontMatter }`. `splitFrontMatter(source)` returns the raw delimited block and
the remaining body as `{ frontMatter, body }`; if the delimiters are missing
or incomplete, `frontMatter` is undefined and `body` contains the original
source.

## Development

Build the local binding used by the test suite, then run the tests:

```sh
npm run local:build
npm test
```

`npm run build` builds the package binding and generated JavaScript/type
declarations. The test suite uses Node's built-in test runner.