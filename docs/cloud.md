# AutoSave in synced cloud folders

> **Last reviewed:** 2026-10-21 · **Last updated:** 2026-10-21 · **Change:** Version history keeps original bytes, follows renames and moves, and uses OneDrive's automatic retention · **Target:** Microsoft Word (Microsoft 365) for Mac 16.113.4

WordCraft uses the desktop sync clients' ordinary folders. It does not connect to a cloud API. A file saved in one of these folders is uploaded by the installed client and becomes available on the person's other devices.

## Folder discovery

On macOS, WordCraft checks `~/Library/Mobile Documents/com~apple~CloudDocs` for iCloud Drive, each `GoogleDrive-<account>` and `OneDrive-<account>` under `~/Library/CloudStorage`, then older `~/Google Drive` and `~/OneDrive` folders. On Windows, iCloud Drive is `%USERPROFILE%\iCloudDrive`. Google Drive may be a drive letter whose root contains `My Drive` (including localized client names), or a mirrored profile folder. OneDrive uses `%OneDriveConsumer%`, `%OneDriveCommercial%` or `%OneDrive%`. On Linux and other systems there is no assumed client path; `WORDCRAFT_CLOUD_ICLOUD`, `WORDCRAFT_CLOUD_GDRIVE` and `WORDCRAFT_CLOUD_ONEDRIVE` can name folders. The browser build has no folders to discover.

The desktop host discovers roots once. `file.cloud={}` reports provider, root, current path, conflict state and discovered roots. Provider ids are `icloud`, `gdrive` and `onedrive`. iCloud-only files must be downloaded by the sync client before opening.

## Save a new document

The Save to Cloud dialog asks where AutoSave should keep a new document. `file.saveToCloud` accepts optional `provider` (`icloud`, `gdrive` or `onedrive`) and `name` (without extension). With no provider, the first discovered root is chosen in iCloud Drive, Google Drive, OneDrive order. The file is written as a new `.docx` in that provider's documents folder, using a numbered name when needed.

This is the same basic model Word uses when a new document is saved to OneDrive: Word chooses a synced location and its client handles upload. WordCraft does not provide Microsoft's service integration or coauthoring features.

## Conflict protection
## Closing and switching documents

With AutoSave enabled, an edited document closes, opens another document or creates a new document after WordCraft writes the pending changes with `file.save` and {"ifUnchanged":true}. The action continues without a Save Changes prompt. If another device changed the file, the Save Conflict dialog stays open and the document remains open. If Save Changes was already visible, a successful AutoSave closes that prompt and continues the pending action. With AutoSave disabled, the existing Save Changes behaviour remains.

A document opened from an iCloud Drive, Google Drive or OneDrive folder is eligible for AutoSave immediately. It does not need an explicit save first. A local document outside those folders still needs an explicit save before AutoSave can write it.

## Version history

The desktop app keeps a local history for each saved document under `<settings_dir>/versions/<path hash>/`, where the hash is taken from the canonical path. On Windows the path is lower-cased first, because file names there ignore case. A folder made by an earlier build under the case-sensitive hash is moved to the new name the first time it is looked up. Each version is the exact byte sequence written to the document's file, stored as `<id>.bin`. The file is flushed and read back before it replaces anything, and it is described in `index.json` by `id`, `savedAt` (UTC), `author`, `size`, `kind` (`save`, `autosave`, `restore` or `original`), `windowStart`, `digest` (SHA-256), `session` and `capturedAt`. `document.json` records the path the folder last belonged to. History is written in the background. A failure in the history is reported and logged. It never stops a save and never changes the bytes saved. Only the document's own file has a history: a copy exported in another format, such as PDF or plain text, does not create one.

Every explicit save creates a version. Word's AutoSave on OneDrive adds a version at the first change after a document opens and then about every ten minutes. WordCraft does the same: the first AutoSave write of each editing session (one opening of the document) starts a new version, and later writes from that session update it until ten minutes have passed since the version was started. The last write before the document closes is therefore always in the history.

Before any save writes over a file whose current bytes are not already in its history, WordCraft stores those bytes as an `original` version. `savedAt` is the file's modification time. This covers a `.docx` made before WordCraft was installed, a file another program changed while it was open or closed, and the file that Save As replaces. The copy is written synchronously, before the file is replaced. The file is read once for each opened document, and later saves only compare the disk stamp.

Retention follows the automatic version limits of SharePoint and OneDrive for work. All versions are kept for 30 days. From 30 to 60 days, the newest version of each hour is kept. From 60 to 180 days, the newest of each day is kept. After that, the newest of each week is kept. Age alone never empties a history. Each document keeps at most 500 versions and 500 MiB, newest first. The oldest `original` version is pinned: it outlives the time rules and is the last version to go under the count and size limits. A personal OneDrive keeps only the last 25 versions; the local history keeps more because it is private to the computer. A damaged `index.json` is read one entry at a time. Version files that the index does not mention are listed again from their own files, so pruning never deletes them only because the index lost them. A version whose bytes no longer match its digest is refused.

The history follows the document. `file.rename` moves its folder to the new name, as OneDrive keeps a file's history through a rename or move. Save As, Save to Cloud and Keep both write a new file whose history starts with that save, and the original file keeps its own history. This is the same as copying a file in OneDrive, where only the latest version comes along. When a file with no history is opened or saved, WordCraft looks for the folder of a file that was moved or renamed outside the app. A folder matches when its recorded path no longer exists and its newest version has exactly the bytes now in the file. A copy, whose original still exists, never takes the history. A file that was both moved and edited outside WordCraft starts a new history with its current bytes as the `original`.

`file.versions={}` lists versions newest first and returns `{id,savedAt,author,size,kind}`. The index is read when the call is made, so the first call after a document opens already returns its versions. `file.versions={"open":"id"}` opens an older version as a separate unsaved document named `Name (version <local date time>)`. `file.versions={"restore":"id"}` replaces the current content in one Undo step. Unsaved changes are first stored as an `autosave` version, so the state before the restore stays in the history. The restored bytes are then recorded at once as a new `restore` version, whether or not AutoSave writes the file afterwards. The listing can also show the `original` kind. The legacy `{"save":"label"}` and numeric `{"restore":index}` forms remain available for compatibility.

File > Info > Version History and the document name in the title bar open the right-hand history pane. Versions are grouped by local day and show local time and author. The pane offers Open version and Restore. An opened older version shows a Previous version bar with Restore and Save a copy.

WordCraft keeps a disk stamp with size, modification time and a fast byte digest. AutoSave calls `file.save` with `ifUnchanged`. If another device or program changed the bytes, or the file is missing because it was moved or deleted, AutoSave does not overwrite it. It records `changed` or `missing` and pauses. A timestamp-only touch with identical bytes is accepted.

The Save Conflict dialog offers **Keep both**, **Keep this version**, **Open the other version**, **Decide later**, and **Save it here again**. `file.resolveConflict` takes `{"keep":"both"}`, `{"keep":"mine"}` or `{"keep":"theirs"}`. `both` writes `Name (conflict copy).docx` beside the other version and edits the copy. `mine` overwrites the path. `theirs` reopens the file on disk and cannot reopen a missing file. When the file is missing, the dialog offers only **Save it here again** (`mine`, which writes the document back to its original path) and **Decide later**.

## Limits

The feature depends on a compatible sync client being installed and signed in. There is no cloud API, server-side merge or real-time coauthoring. The version history described above is local to the desktop profile, is not synchronized between devices and is unavailable in the browser. iCloud Drive, Google Drive and OneDrive keep their own server-side histories; WordCraft does not browse or merge those histories.

## Revision history

| Date | Change |
|---|---|
| 2026-10-21 | Version history storage: original bytes are kept before the first overwrite and external changes are kept before they are replaced; AutoSave starts a version for each editing session; the history follows `file.rename` and files moved outside the app; retention follows SharePoint's automatic limits (500 versions) with a pinned original; Restore records its version at once; digests and index recovery; case-insensitive history keys on Windows; the first listing after opening is complete. |
| 2026-10-17 | Added Word-style close and switch behaviour, immediate AutoSave coverage for synced folders and local version history commands. |
| 2026-10-16 | Added cloud-folder discovery, Save to Cloud, disk-stamp conflict protection and resolution commands. |
