CREATE VIRTUAL TABLE records_fts USING fts5(
  body, title, tags,
  content='',
  tokenize = 'unicode61'
);

-- Row-level sync: triggers add/remove FTS rows in lockstep with records.
CREATE TRIGGER records_fts_ai AFTER INSERT ON records
WHEN NEW.tombstoned = 0
BEGIN
  INSERT INTO records_fts(rowid, body, title, tags)
  VALUES (NEW.rowid, NEW.body, COALESCE(json_extract(NEW.taxonomy, '$.title'), ''),
          COALESCE(json_extract(NEW.taxonomy, '$.tags'), ''));
END;

CREATE TRIGGER records_fts_ad AFTER DELETE ON records
BEGIN
  INSERT INTO records_fts(records_fts, rowid, body, title, tags)
  VALUES ('delete', OLD.rowid, OLD.body, '', '');
END;

CREATE TRIGGER records_fts_au AFTER UPDATE ON records
BEGIN
  INSERT INTO records_fts(records_fts, rowid, body, title, tags)
  VALUES ('delete', OLD.rowid, OLD.body, '', '');
  INSERT INTO records_fts(rowid, body, title, tags)
  VALUES (NEW.rowid, NEW.body, COALESCE(json_extract(NEW.taxonomy, '$.title'), ''),
          COALESCE(json_extract(NEW.taxonomy, '$.tags'), ''));
END;
