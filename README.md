# 8-Bit Rebellion on the web

This project runs *Linkin Park 8-Bit Rebellion!* (iPhone, version 1.4.5) in a web browser, with its online features restored by a replacement game server.

It runs the original game code in [touchHLE](https://github.com/touchHLE/touchHLE), an iPhone OS emulator, compiled to WebAssembly. The game's own files are not included: each player chooses their own copy of the game, or the operator of a private server provides it.

> [!NOTE]
> This is a fan-made compatibility project. It isn't affiliated with or endorsed by Linkin Park, Warner Records or Artificial Life, Inc. Their names and trademarks are used only to describe compatibility. You need your own copy of the game.

## How it works

| Part | Location | What it does |
|---|---|---|
| Emulator | `touchHLE/` | touchHLE with the [8BeetBebellion](https://github.com/enemykin/8BeetBebellion) compatibility patch, ported to WebAssembly: a portable ARM interpreter (`src/cpu/arm_interp`), OpenGL ES 1.1 on WebGL 2 (`src/gles/gles1_on_gles3.rs`), Asyncify coroutines (`src/coroutine.rs`), and socket streams tunnelled over WebSocket (`src/frameworks/core_foundation/cf_stream.rs`). |
| Web page | `web/` | Loads the emulator and the IPA, keeps saves in IndexedDB, plays movies and bridges network connections. |
| Server | `server/` | Serves the page, converts the game's movies for browsers, optionally serves the IPA, and replaces the dead game server at `lp110.m-botme.com/lp/httpGDServlet`. |
| Protocol | `docs/protocol.md` | The game's online protocol, reverse engineered from the binary. |

## Requirements

- [Docker](https://www.docker.com/), to build the WebAssembly emulator.
- [Node.js](https://nodejs.org/) 24 or later, for the server.
- [ffmpeg](https://ffmpeg.org/) on the server's `PATH`, to convert the game's movies for browsers.
- A copy of the game: the iPhone version 1.4.5 IPA (bundle ID `com.alife.linkinpark`).

## Run it

1. Build the emulator. The first build downloads the toolchain image and takes several minutes:

   ```sh
   scripts/build-web.sh
   ```

2. Decide where the game file comes from (see [Game file modes](#game-file-modes)). For a private server, put the IPA at `game/8bitrebellion.ipa` or set `GAME_IPA` to its path. Otherwise, skip this step.
3. Install the server's dependencies and start it:

   ```sh
   cd server
   npm install
   npm start
   ```

4. Open `http://localhost:8080/`. If the server doesn't provide the game, click **Choose game file** and select your IPA. Then click **Play**.

## Game file modes

The page picks a mode automatically, depending on whether the server has the IPA:

| Mode | When | What happens |
|---|---|---|
| Player-supplied | The server has no IPA. Use this for any public server. | Each player chooses their own IPA. It stays in their browser (IndexedDB), so they choose it only once. The page sends only the game's three movies to the server, which converts them for playback and doesn't keep them. |
| Server-hosted | The server has an IPA at `GAME_IPA`. | The server sends the IPA to every visitor and converts and caches the movies. Only use this on a private server for people who own the game: on a public server it would distribute the game. |

The server reads these environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `PORT` | `8080` | HTTP port. |
| `GAME_IPA` | `game/8bitrebellion.ipa` | Path to an IPA to host. If there's no file there, players supply their own. |
| `GAME_DB` | `server/data/game.sqlite` | Game database. Converted movies are cached next to it. |
| `GAME_DEBUG` | unset | Set to log every game message. |

## Controls

Click or tap where you would touch the iPhone screen. On a keyboard, **A**/**D** or the arrow keys move, **Space** attacks, **W** or **Up** enters a door, and **Escape** opens the menu.

## Online features

The server implements the original protocol: guest play, accounts, presence of other players in each location, friends, chat, mail, bulletin boards, the store, achievements, apartments, ranks and ratings. Data is stored in SQLite. See `docs/protocol.md` for the details and the server's choices where the original behavior is unknown.

## Development

- Server tests: `node --test server/game/game.test.mjs`
- Interpreter tests: `cargo test --release` in `touchHLE/src/cpu/arm_interp`
- Native build, useful for debugging with dynarmic as a reference CPU: see `touchHLE/dev-docs/building.md`. On Windows with CMake 4, set `CMAKE_POLICY_VERSION_MINIMUM=3.5`. Set `TOUCHHLE_GAME_SERVER=127.0.0.1:8080` to connect a native build to the local server, and `TOUCHHLE_SCRIPTED_TAPS="seconds:x,y;..."` to script taps in game points.
- Extra emscripten flags for debugging builds: `TOUCHHLE_EMCC_FLAGS="-sSAFE_HEAP -sASSERTIONS=2" scripts/build-web.sh`

## License

This project is licensed under the [Mozilla Public License 2.0](LICENSE). It includes [touchHLE](https://github.com/touchHLE/touchHLE) and the [8BeetBebellion](https://github.com/enemykin/8BeetBebellion) compatibility patch, both under the same license, plus libraries and resources under their own licenses. Builds of the emulator are distributed under the GNU General Public License, version 3 or later, like touchHLE's. See [NOTICE](NOTICE) for the details and credits.

The game and its content belong to their respective rights holders and are not part of this repository.
