// Persistent game data in SQLite.
import { DatabaseSync } from "node:sqlite";

const SCHEMA = `
CREATE TABLE IF NOT EXISTS accounts (
  account_id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  password TEXT NOT NULL,
  email TEXT NOT NULL,
  data TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS players (
  player_id INTEGER PRIMARY KEY AUTOINCREMENT,
  account_id INTEGER,
  is_guest INTEGER NOT NULL,
  profile_name TEXT NOT NULL DEFAULT '' COLLATE NOCASE,
  profile TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS players_account ON players (account_id);
CREATE TABLE IF NOT EXISTS items (
  real_id INTEGER PRIMARY KEY AUTOINCREMENT,
  player_id INTEGER NOT NULL,
  item_id INTEGER NOT NULL,
  item_type INTEGER NOT NULL,
  description TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS items_player ON items (player_id);
CREATE TABLE IF NOT EXISTS achievements (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  player_id INTEGER NOT NULL,
  achievement_id INTEGER NOT NULL,
  value TEXT NOT NULL,
  timestamp TEXT NOT NULL,
  UNIQUE (player_id, achievement_id)
);
CREATE TABLE IF NOT EXISTS apartments (
  player_id INTEGER PRIMARY KEY,
  data BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS friends (
  player_id INTEGER NOT NULL,
  friend_id INTEGER NOT NULL,
  top INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (player_id, friend_id)
);
CREATE TABLE IF NOT EXISTS blocks (
  player_id INTEGER NOT NULL,
  target_id INTEGER NOT NULL,
  PRIMARY KEY (player_id, target_id)
);
CREATE TABLE IF NOT EXISTS requests (
  request_id INTEGER PRIMARY KEY AUTOINCREMENT,
  from_id INTEGER NOT NULL,
  target_id INTEGER NOT NULL,
  type INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS mail (
  mail_id INTEGER PRIMARY KEY AUTOINCREMENT,
  receiver_id INTEGER NOT NULL,
  sender_id INTEGER NOT NULL,
  sender_name TEXT NOT NULL,
  chain_id INTEGER NOT NULL,
  message BLOB NOT NULL,
  subject TEXT NOT NULL,
  created TEXT NOT NULL,
  updated TEXT NOT NULL,
  read INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS mail_receiver ON mail (receiver_id);
CREATE TABLE IF NOT EXISTS bulletin (
  message_id INTEGER PRIMARY KEY AUTOINCREMENT,
  location_id INTEGER NOT NULL,
  player_id INTEGER NOT NULL,
  player_name TEXT NOT NULL,
  message TEXT NOT NULL,
  timestamp TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS bulletin_location ON bulletin (location_id);
CREATE TABLE IF NOT EXISTS ratings (
  rater_id INTEGER NOT NULL,
  target_id INTEGER NOT NULL,
  rating INTEGER NOT NULL,
  PRIMARY KEY (rater_id, target_id)
);
CREATE TABLE IF NOT EXISTS news (
  news_id INTEGER PRIMARY KEY AUTOINCREMENT,
  created TEXT NOT NULL,
  subject TEXT NOT NULL,
  content TEXT NOT NULL,
  link TEXT NOT NULL DEFAULT ''
);
`;

export class Store {
  constructor(path) {
    this.db = new DatabaseSync(path);
    this.db.exec("PRAGMA journal_mode = WAL");
    this.db.exec(SCHEMA);
    this.cache = new Map();
  }

  q(sql) {
    let stmt = this.cache.get(sql);
    if (!stmt) {
      stmt = this.db.prepare(sql);
      this.cache.set(sql, stmt);
    }
    return stmt;
  }

  // Accounts
  createAccount(account) {
    const { lastInsertRowid } = this.q(
      "INSERT INTO accounts (user_name, password, email, data) VALUES (?, ?, ?, ?)",
    ).run(account.userName, account.password, account.email, JSON.stringify(account));
    return Number(lastInsertRowid);
  }
  accountByName(name) {
    return this.q("SELECT * FROM accounts WHERE user_name = ?").get(name);
  }
  emailTaken(email) {
    return !!email && !!this.q("SELECT 1 FROM accounts WHERE email = ? COLLATE NOCASE").get(email);
  }
  accountById(id) {
    return this.q("SELECT * FROM accounts WHERE account_id = ?").get(id);
  }
  updateAccountData(id, account) {
    this.q("UPDATE accounts SET data = ? WHERE account_id = ?").run(JSON.stringify(account), id);
  }

  // Players. A player row exists for every guest and every created profile.
  createPlayer(accountId, isGuest, profile) {
    const { lastInsertRowid } = this.q(
      "INSERT INTO players (account_id, is_guest, profile_name, profile) VALUES (?, ?, ?, ?)",
    ).run(accountId, isGuest ? 1 : 0, profile.profileName ?? "", JSON.stringify(profile));
    return Number(lastInsertRowid);
  }
  player(id) {
    const row = this.q("SELECT * FROM players WHERE player_id = ?").get(id);
    return row && { ...row, profile: JSON.parse(row.profile) };
  }
  playerByAccount(accountId) {
    const row = this.q("SELECT * FROM players WHERE account_id = ? AND is_guest = 0").get(accountId);
    return row && { ...row, profile: JSON.parse(row.profile) };
  }
  saveProfile(id, profile) {
    this.q("UPDATE players SET profile_name = ?, profile = ? WHERE player_id = ?").run(
      profile.profileName ?? "",
      JSON.stringify(profile),
      id,
    );
  }
  searchPlayers(name, limit = 20) {
    return this.q(
      "SELECT player_id FROM players WHERE profile_name LIKE ? AND profile_name != '' LIMIT ?",
    )
      .all(`%${name}%`, limit)
      .map((r) => r.player_id);
  }
  topPlayers(limit) {
    return this.q("SELECT player_id, profile FROM players WHERE profile_name != ''")
      .all()
      .map((r) => ({ id: r.player_id, reputation: JSON.parse(r.profile).reputation ?? 0 }))
      .sort((a, b) => b.reputation - a.reputation || a.id - b.id)
      .slice(0, limit);
  }

  // Items
  addItem(playerId, item) {
    const { lastInsertRowid } = this.q(
      "INSERT INTO items (player_id, item_id, item_type, description) VALUES (?, ?, ?, ?)",
    ).run(playerId, item.itemID, item.itemType, item.description ?? "");
    return Number(lastInsertRowid);
  }
  deleteItem(playerId, realId) {
    this.q("DELETE FROM items WHERE player_id = ? AND real_id = ?").run(playerId, realId);
  }
  items(playerId) {
    return this.q("SELECT * FROM items WHERE player_id = ?").all(playerId);
  }

  // Achievements
  addAchievement(playerId, a) {
    this.q(
      `INSERT INTO achievements (player_id, achievement_id, value, timestamp) VALUES (?, ?, ?, ?)
       ON CONFLICT (player_id, achievement_id) DO UPDATE SET value = excluded.value, timestamp = excluded.timestamp`,
    ).run(playerId, a.achievementID, a.achievementValue ?? "", a.timestamp ?? "");
    return this.q("SELECT id FROM achievements WHERE player_id = ? AND achievement_id = ?").get(
      playerId,
      a.achievementID,
    ).id;
  }
  achievements(playerId) {
    return this.q("SELECT * FROM achievements WHERE player_id = ?").all(playerId);
  }

  // Apartments
  setApartment(playerId, data) {
    this.q(
      "INSERT INTO apartments (player_id, data) VALUES (?, ?) ON CONFLICT (player_id) DO UPDATE SET data = excluded.data",
    ).run(playerId, data);
  }
  apartment(playerId) {
    return this.q("SELECT data FROM apartments WHERE player_id = ?").get(playerId)?.data;
  }

  // Friends and blocks
  addFriends(a, b) {
    const stmt = this.q("INSERT OR IGNORE INTO friends (player_id, friend_id) VALUES (?, ?)");
    stmt.run(a, b);
    stmt.run(b, a);
  }
  removeFriends(a, b) {
    const stmt = this.q("DELETE FROM friends WHERE player_id = ? AND friend_id = ?");
    stmt.run(a, b);
    stmt.run(b, a);
  }
  friends(playerId) {
    return this.q("SELECT friend_id FROM friends WHERE player_id = ?")
      .all(playerId)
      .map((r) => r.friend_id);
  }
  friendRows(playerId) {
    return this.q("SELECT friend_id, top FROM friends WHERE player_id = ?").all(playerId);
  }
  setTopFriend(playerId, friendId, top) {
    this.q("UPDATE friends SET top = ? WHERE player_id = ? AND friend_id = ?").run(top ? 1 : 0, playerId, friendId);
  }
  block(playerId, targetId) {
    this.q("INSERT OR IGNORE INTO blocks (player_id, target_id) VALUES (?, ?)").run(playerId, targetId);
  }
  unblock(playerId, targetId) {
    this.q("DELETE FROM blocks WHERE player_id = ? AND target_id = ?").run(playerId, targetId);
  }
  blocked(playerId) {
    return this.q("SELECT target_id FROM blocks WHERE player_id = ?")
      .all(playerId)
      .map((r) => r.target_id);
  }

  // Friend requests (chat requests and pokes are not stored)
  addRequest(fromId, targetId, type) {
    const { lastInsertRowid } = this.q(
      "INSERT INTO requests (from_id, target_id, type) VALUES (?, ?, ?)",
    ).run(fromId, targetId, type);
    return Number(lastInsertRowid);
  }
  request(id) {
    return this.q("SELECT * FROM requests WHERE request_id = ?").get(id);
  }
  deleteRequest(id) {
    this.q("DELETE FROM requests WHERE request_id = ?").run(id);
  }
  requestsFor(targetId) {
    return this.q("SELECT * FROM requests WHERE target_id = ?").all(targetId);
  }

  // Mail
  addMail(m) {
    const { lastInsertRowid } = this.q(
      `INSERT INTO mail (receiver_id, sender_id, sender_name, chain_id, message, subject, created, updated)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
    ).run(m.receiverId, m.senderId, m.senderName, m.chainId, m.message, m.subject, m.created, m.created);
    return Number(lastInsertRowid);
  }
  mailFor(playerId) {
    return this.q("SELECT * FROM mail WHERE receiver_id = ? ORDER BY mail_id").all(playerId);
  }
  mailById(id) {
    return this.q("SELECT * FROM mail WHERE mail_id = ?").get(id);
  }
  markMailRead(playerId, mailId) {
    this.q("UPDATE mail SET read = 1 WHERE receiver_id = ? AND mail_id = ?").run(playerId, mailId);
  }
  deleteMail(playerId, mailId) {
    this.q("DELETE FROM mail WHERE receiver_id = ? AND mail_id = ?").run(playerId, mailId);
  }

  // Bulletin boards
  addBulletin(b) {
    this.q(
      "INSERT INTO bulletin (location_id, player_id, player_name, message, timestamp) VALUES (?, ?, ?, ?, ?)",
    ).run(b.locationId, b.playerId, b.playerName, b.message, b.timestamp);
  }
  bulletins(locationId, limit = 50) {
    return this.q(
      "SELECT * FROM bulletin WHERE location_id = ? ORDER BY message_id DESC LIMIT ?",
    ).all(locationId, limit);
  }

  // Ratings
  addRating(raterId, targetId, rating) {
    const { changes } = this.q(
      "INSERT OR IGNORE INTO ratings (rater_id, target_id, rating) VALUES (?, ?, ?)",
    ).run(raterId, targetId, rating);
    return changes > 0;
  }
  ratingCounts(targetId) {
    return this.q("SELECT rating, COUNT(*) AS n FROM ratings WHERE target_id = ? GROUP BY rating").all(
      targetId,
    );
  }

  // News
  news() {
    return this.q("SELECT * FROM news ORDER BY news_id DESC").all();
  }
  addNews(subject, content, created, link = "") {
    this.q("INSERT INTO news (created, subject, content, link) VALUES (?, ?, ?, ?)").run(
      created,
      subject,
      content,
      link,
    );
  }
}
