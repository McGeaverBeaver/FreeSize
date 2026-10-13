<?php
// FreeSize for Unraid: what the page asks for. Nothing here deletes
// anything on its own: moving to the quarantine is a rename, a share move
// keeps the originals in the quarantine, and only "empty" and Docker
// cleanup remove data, each after the page listed exactly what goes.

require_once __DIR__.'/common.php';

if ($_SERVER['REQUEST_METHOD'] !== 'POST') dt_fail('POST only', 405);
dt_check_csrf();

/** Replies with the command's JSON output, or its message on failure. */
function dt_json($result) {
  if ($result['ok']) {
    $decoded = json_decode($result['output'], true);
    if ($decoded !== null) dt_reply(['ok' => true, 'result' => $decoded]);
  }
  dt_reply($result, $result['ok'] ? 200 : 400);
}

function dt_disk($name) {
  $disk = (string)($_POST[$name] ?? '');
  if (!preg_match('/^[a-z][a-z0-9_]{0,30}$/', $disk)) dt_fail('choose a disk');
  return $disk;
}

/** A share, and a folder in it (empty for all of it). */
function dt_share_folder() {
  $share = dt_text('share', 100);
  $folder = trim(dt_text('folder', 500), '/');
  if ($share === '' || str_contains($share, '/')) dt_fail('choose a share');
  // With `=`, a name starting with a dash isn't taken for an option.
  return ["--share=$share", "--folder=$folder"];
}

function dt_move_args() {
  $froms = $_POST['from'] ?? [];
  if (!is_array($froms) || !$froms || count($froms) > 40) dt_fail('choose the disks to move from');
  $args = dt_share_folder();
  foreach ($froms as $from) {
    if (!preg_match('/^[a-z][a-z0-9_]{0,30}$/', (string)$from)) dt_fail('choose a disk');
    array_push($args, '--from', (string)$from);
  }
  $args = array_merge($args, ['--to', dt_disk('to')]);
  if (($_POST['empty'] ?? '') === '1') $args[] = '--empty-after';
  return $args;
}

/** A preset's kind and what it works on: empty-disk DISK, or gather SHARE [--to DISK]. */
function dt_preset_args() {
  $kind = (string)($_POST['kind'] ?? '');
  if ($kind === 'empty-disk') return ['empty-disk', dt_disk('disk')];
  if ($kind === 'gather') {
    $share = dt_text('share', 100);
    if ($share === '' || str_contains($share, '/') || $share[0] === '.') dt_fail('choose a share');
    $args = ['gather', "--", $share];
    if (($_POST['to'] ?? '') !== '') array_splice($args, 1, 0, ['--to', dt_disk('to')]);
    return $args;
  }
  dt_fail('choose a preset');
}

switch ($_POST['action'] ?? '') {
case 'scan':
  dt_reply(dt_run(['unraid', 'scan']));

case 'estimate':
  dt_json(dt_run(array_merge(['unraid', 'estimate', '--json', '--'], dt_paths('paths'))));

case 'quarantine_add':
  $args = ['unraid', 'quarantine', 'add', '--reason', dt_text('reason'), '--by', 'Unraid web UI', '--'];
  dt_json(dt_run(array_merge($args, dt_paths('paths'))));

case 'quarantine_restore':
  dt_reply(dt_run(['unraid', 'quarantine', 'restore', dt_id('batch')]));

case 'quarantine_empty':
  $batches = $_POST['batches'] ?? [];
  if (!is_array($batches) || !$batches) dt_fail('choose what to empty');
  $ids = [];
  foreach ($batches as $batch) {
    if (!preg_match('/^[A-Za-z0-9]{1,40}$/', (string)$batch)) dt_fail('no such batch');
    $ids[] = (string)$batch;
  }
  dt_reply(dt_run(array_merge(['unraid', 'quarantine', 'empty', '--'], $ids)));

case 'junk_clean':
  // A trash folder's rule id from the status (letters, digits, dashes).
  $rule = (string)($_POST['rule'] ?? '');
  if (!preg_match('/^[a-z0-9-]{1,40}$/', $rule)) dt_fail('no such rule');
  dt_reply(dt_run(['unraid', 'junk', 'clean', $rule]));

case 'docker_cleanup':
  $items = $_POST['items'] ?? [];
  if (!is_array($items) || !$items) dt_fail('choose what to remove');
  foreach ($items as $item) {
    if (!preg_match('#^[A-Za-z0-9:._/-]{1,200}$#', (string)$item)) dt_fail('no such item');
  }
  dt_reply(dt_run(array_merge(['unraid', 'docker-cleanup', '--'], array_map('strval', $items))));

case 'move_plan':
  dt_json(dt_run(array_merge(['unraid', 'move', 'plan', '--json'], dt_move_args())));

case 'move_start':
  dt_reply(dt_run(array_merge(['unraid', 'move', 'start'], dt_move_args())));

case 'folders':
  $args = array_merge(['unraid', 'folders', '--json'], dt_share_folder());
  if (($_POST['wake'] ?? '') === 'yes') $args[] = '--wake';
  dt_json(dt_run($args));

case 'move_cancel':
  dt_reply(dt_run(['unraid', 'move', 'cancel', dt_id('id')]));

case 'move_preset':
  dt_json(dt_run(array_merge(['unraid', 'move', 'preset', '--json'], dt_preset_args())));

case 'settings':
  $hours = (int)($_POST['scan_hours'] ?? 24);
  $days = (int)($_POST['wake_days'] ?? 7);
  if ($hours < 1 || $hours > 720) dt_fail('scan every 1 to 720 hours');
  if ($days < 0 || $days > 365) dt_fail('wake a disk after 0 to 365 days');
  $yes = fn($k) => ($_POST[$k] ?? '') === 'yes' ? 'yes' : 'no';
  $cfg = "SCAN_HOURS=\"$hours\"\nWAKE_DAYS=\"$days\"\nPERMISSIONS=\"{$yes('permissions')}\"\nREPORT=\"{$yes('report')}\"\n";
  @mkdir($dt_config, 0755, true);
  if (@file_put_contents("$dt_config/freesize.cfg", $cfg) === false) dt_fail("couldn't save the settings");
  dt_request('settings');
  dt_reply(['ok' => true, 'output' => 'Saved.']);

case 'keep':
  // Paths never offered, on top of what FreeSize keeps on its own.
  $lines = preg_split('/\r?\n/', (string)($_POST['paths'] ?? ''));
  $keep = [];
  foreach ($lines as $line) {
    $line = rtrim(trim(preg_replace('/[\x00-\x1f\x7f]/', '', $line)), '/');
    if ($line === '') continue;
    if ($line[0] !== '/') dt_fail("$line: start each path with /");
    $keep[] = $line;
  }
  if (count($keep) > 500) dt_fail('500 paths at most');
  @mkdir($dt_config, 0755, true);
  $text = $keep ? implode("\n", array_unique($keep))."\n" : '';
  if (@file_put_contents("$dt_config/keep.txt", $text) === false) dt_fail("couldn't save the list");
  dt_reply(['ok' => true, 'output' => count($keep) ? 'Saved; those paths are never offered now.' : 'Saved; the list is empty.']);

case 'pair':
  $link = trim((string)($_POST['link'] ?? ''));
  if (!preg_match('#^freesize://pair\?[A-Za-z0-9%._~=&+-]{1,1000}$#', $link)) {
    dt_log('Pairing with the Hub failed: the pasted text isn\'t a whole pairing link');
    dt_fail('paste the whole pairing link from the dashboard (Pair a device); it starts with freesize://pair?');
  }
  $args = ['agent', 'pair', $link, '--state', dt_state()];
  $name = dt_text('name', 80);
  if ($name !== '') array_push($args, '--name', $name);
  $result = dt_run($args, false);
  // The command warns first (a plain http Hub) and gives its reason last.
  // The page shows the reason first, and the log keeps every attempt; the
  // link itself is never logged.
  $said = [];
  $warnings = [];
  foreach ($result['output'] === '' ? [] : explode("\n", $result['output']) as $line) {
    if (str_starts_with($line, 'warning: ')) $warnings[] = $line;
    else $said[] = preg_replace('/^freesize: /', '', $line);
  }
  if ($result['ok']) {
    dt_log($said[0] ?? 'Paired with the Hub.');
    dt_request('hub');
  } else {
    dt_log('Pairing with the Hub failed: '.implode(' ', $said));
  }
  $result['output'] = implode("\n", array_merge($said, $warnings));
  dt_reply($result, $result['ok'] ? 200 : 400);

case 'approver_check':
case 'approver_set':
  $key = strtoupper(trim((string)($_POST['fingerprint'] ?? '')));
  if (!preg_match('/^[A-Z0-9]{4}(-[A-Z0-9]{4}){3}$/', $key)) {
    dt_fail('type the key as the FreeSize app shows it, like ABCD-EFGH-JKMN-PQRS');
  }
  $flag = $_POST['action'] === 'approver_set' ? '--yes' : '--check';
  $result = dt_run(['approver', 'set', $key, $flag, '--state', dt_state()], false);
  if ($result['ok'] && $flag === '--yes') dt_request('refresh');
  dt_reply($result, $result['ok'] ? 200 : 400);

case 'approver_clear':
  dt_reply(dt_run(['approver', 'clear', '--state', dt_state()], false));

case 'revoke':
  dt_reply(dt_run(['agent', 'revoke', '--state', dt_state()], false));

case 'unpair':
  dt_reply(dt_run(['agent', 'unpair', '--state', dt_state()], false));

default:
  dt_fail('unknown action');
}
