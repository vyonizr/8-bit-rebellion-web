// JavaScript functions imported by touchHLE's emscripten build (see
// touchHLE/src/web.rs). Linked with --js-library.
addToLibrary({
  // Return to the browser event loop and resume as soon as possible.
  // MessageChannel avoids the 4ms minimum of nested setTimeout calls.
  touchhle_yield__async: true,
  touchhle_yield: () =>
    Asyncify.handleSleep((wakeUp) => {
      const channel = new MessageChannel();
      channel.port1.onmessage = () => wakeUp();
      channel.port2.postMessage(0);
    }),
});

// Movie playback for MPMoviePlayerController (see
// touchHLE/src/frameworks/media_player/movie_player.rs). The movie plays in
// the page's <video id="video"> element; a click or tap skips it.
addToLibrary({
  $touchhleVideo: { finished: false, url: null },
  touchhle_video_play__deps: ["$touchhleVideo"],
  touchhle_video_play: (data, len, muted) => {
    const video = document.getElementById("video");
    const blob = new Blob([HEAPU8.slice(data, data + len)], { type: "video/mp4" });
    touchhleVideo.url = URL.createObjectURL(blob);
    touchhleVideo.finished = false;
    const finish = () => {
      touchhleVideo.finished = true;
    };
    video.onended = finish;
    video.onerror = finish;
    video.onclick = finish;
    video.muted = !!muted;
    video.src = touchhleVideo.url;
    video.hidden = false;
    video.play().catch(finish);
  },
  touchhle_video_finished__deps: ["$touchhleVideo"],
  touchhle_video_finished: () => touchhleVideo.finished,
  touchhle_video_stop__deps: ["$touchhleVideo"],
  touchhle_video_stop: () => {
    const video = document.getElementById("video");
    video.pause();
    video.hidden = true;
    video.removeAttribute("src");
    video.load();
    if (touchhleVideo.url) URL.revokeObjectURL(touchhleVideo.url);
    touchhleVideo.url = null;
  },
  touchhle_video_set_muted: (muted) => {
    document.getElementById("video").muted = !!muted;
  },
});

// TCP connections for CFStream socket streams (see
// touchHLE/src/frameworks/core_foundation/cf_stream.rs), tunnelled through a
// WebSocket to the page's server. States: 0 connecting, 1 open, 2 closed by
// the peer, 3 error.
addToLibrary({
  $touchhleNet: { sockets: new Map(), nextId: 1 },
  touchhle_net_open__deps: ["$touchhleNet"],
  touchhle_net_open: (hostPtr, hostLen, port) => {
    const host = UTF8ToString(hostPtr, hostLen);
    const scheme = location.protocol === "https:" ? "wss" : "ws";
    const url = `${scheme}://${location.host}/net?host=${encodeURIComponent(host)}&port=${port}`;
    const socket = { ws: new WebSocket(url), state: 0, chunks: [], offset: 0 };
    socket.ws.binaryType = "arraybuffer";
    socket.ws.onopen = () => (socket.state = 1);
    socket.ws.onmessage = (event) => socket.chunks.push(new Uint8Array(event.data));
    socket.ws.onerror = () => (socket.state = 3);
    socket.ws.onclose = () => {
      if (socket.state !== 3) socket.state = socket.state === 1 ? 2 : 3;
    };
    const id = touchhleNet.nextId++;
    touchhleNet.sockets.set(id, socket);
    return id;
  },
  touchhle_net_state__deps: ["$touchhleNet"],
  touchhle_net_state: (id) => touchhleNet.sockets.get(id)?.state ?? 3,
  touchhle_net_recv__deps: ["$touchhleNet"],
  touchhle_net_recv: (id, buf, len) => {
    const socket = touchhleNet.sockets.get(id);
    if (!socket || socket.chunks.length === 0) return 0;
    const chunk = socket.chunks[0];
    const n = Math.min(len, chunk.length - socket.offset);
    HEAPU8.set(chunk.subarray(socket.offset, socket.offset + n), buf);
    socket.offset += n;
    if (socket.offset === chunk.length) {
      socket.chunks.shift();
      socket.offset = 0;
    }
    return n;
  },
  touchhle_net_send__deps: ["$touchhleNet"],
  touchhle_net_send: (id, buf, len) => {
    const socket = touchhleNet.sockets.get(id);
    if (socket?.state === 1) socket.ws.send(HEAPU8.slice(buf, buf + len));
  },
  touchhle_net_close__deps: ["$touchhleNet"],
  touchhle_net_close: (id) => {
    touchhleNet.sockets.get(id)?.ws.close();
    touchhleNet.sockets.delete(id);
  },
});
