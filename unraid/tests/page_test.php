<?php
// Tests for the Unraid page's action.php, run as Unraid runs it: behind a
// copy of Unraid's own token check, with a stand-in `freesize` command.
// Run: php unraid/tests/page_test.php

$action = realpath(__DIR__.'/../plugin/root/usr/local/emhttp/plugins/freesize/include/action.php');
$root = sys_get_temp_dir().'/freesize-page-test-'.getmypid();
$failures = 0;

function check($ok, $what) {
  global $failures;
  echo ($ok ? 'ok:   ' : 'FAIL: ').$what."\n";
  if (!$ok) $failures++;
}

function reset_root() {
  global $root;
  exec('rm -rf '.escapeshellarg($root));
  foreach (['var/local/emhttp', 'var/log', 'boot/config/plugins/freesize'] as $dir) {
    mkdir("$root/$dir", 0755, true);
  }
  file_put_contents("$root/var/local/emhttp/var.ini", "csrf_token=\"T0KEN\"\n");
  // The stand-in command: remembers its arguments and answers as told.
  $bin = "$root/freesize";
  file_put_contents($bin, <<<'SH'
#!/bin/sh
printf '%s\n' "$@" > "$FREESIZE_TEST_ROOT/args"
case "$DT_STUB" in
  paired)
    echo "warning: http://hub.lan:9090 is plain http, so what this machine reports can be read on the network." >&2
    echo 'Paired with Test Hub as "Tower".'
    echo "This machine's key: AAAA-BBBB-CCCC-DDDD"
    ;;
  refused)
    echo "warning: http://hub.lan:9090 is plain http, so what this machine reports can be read on the network." >&2
    echo "freesize: wrong or expired pairing code" >&2
    exit 1
    ;;
esac
SH);
  chmod($bin, 0755);
}

/** Posts to action.php; returns [status, decoded reply, raw output]. */
function post($fields, $prelude, $stub = '') {
  global $root, $action;
  $env = [
    'DT_REQUEST' => json_encode(['post' => $fields, 'prelude' => $prelude]),
    'DT_ACTION' => $action,
    'DT_STUB' => $stub,
    'FREESIZE_TEST_ROOT' => $root,
    'FREESIZE_TEST_BIN' => "$root/freesize",
    'PATH' => getenv('PATH'),
  ];
  if (getenv('LD_LIBRARY_PATH')) $env['LD_LIBRARY_PATH'] = getenv('LD_LIBRARY_PATH');
  $process = proc_open([PHP_BINARY, '-n', __DIR__.'/request.php'], [1 => ['pipe', 'w'], 2 => ['pipe', 'w']], $pipes, null, $env);
  $out = stream_get_contents($pipes[1]);
  $err = stream_get_contents($pipes[2]);
  proc_close($process);
  $status = preg_match('/status (\d+)/', $err, $m) ? (int)$m[1] : 0;
  return [$status, json_decode($out, true), $out];
}

function log_text() {
  global $root;
  return (string)@file_get_contents("$root/var/log/freesize.log");
}

$link = 'freesize://pair?hub=http%3A%2F%2Fhub.lan%3A9090&code=AAAA-BBBB-CCCC&key=ABCD-EFGH-JKMN-PQRS';

// Unraid removes the token before the page's script runs: the page must
// still work, which it didn't before 0.8.5.
reset_root();
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'pair', 'link' => $link, 'name' => 'Tower'], true, 'paired');
check($status === 200 && ($reply['ok'] ?? false), 'pairing works behind Unraid\'s token check');
$args = explode("\n", trim((string)@file_get_contents("$root/args")));
check($args === ['agent', 'pair', $link, '--state', "$root/boot/config/plugins/freesize/agent.json", '--name', 'Tower'], 'the command gets the link, the plugin\'s pairing file and the name');
check(file_exists("$root/var/local/freesize/requests/hub"), 'the background part is told to report');
check(str_contains(log_text(), 'Paired with Test Hub as "Tower".'), 'the log says it paired');
check(!str_contains(log_text(), 'AAAA-BBBB-CCCC'), 'the log never holds the pairing code');

// Other buttons go through the same check.
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'keep', 'paths' => "/mnt/user/Backups\n"], true);
check($status === 200 && ($reply['ok'] ?? false), 'saving the never-touch list works behind Unraid\'s token check');
check(file_get_contents("$root/boot/config/plugins/freesize/keep.txt") === "/mnt/user/Backups\n", 'the list is saved');

// A wrong token never reaches the page's script.
reset_root();
[$status, $reply, $out] = post(['csrf_token' => 'WRONG', 'action' => 'pair', 'link' => $link], true, 'paired');
check(($reply['error'] ?? '') === 'wrong csrf_token', 'Unraid stops a wrong token');
check(!file_exists("$root/args"), 'the command never ran');

// A failed pairing: the reason first, the warning after it, and logged.
reset_root();
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'pair', 'link' => $link], true, 'refused');
check($status === 400 && !($reply['ok'] ?? true), 'a refused pairing fails');
$lines = explode("\n", $reply['output'] ?? '');
check(($lines[0] ?? '') === 'wrong or expired pairing code', 'the reason comes first');
check(str_starts_with($lines[1] ?? '', 'warning: http://hub.lan:9090 is plain http'), 'the warning comes after it');
check(str_contains(log_text(), 'Pairing with the Hub failed: wrong or expired pairing code'), 'the log says why it failed');
check(!file_exists("$root/var/local/freesize/requests/hub"), 'the background part isn\'t told anything');

// Text that isn't a whole link is refused before the command runs.
reset_root();
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'pair', 'link' => 'freesize://pair?hub=http://hub.lan'], true);
check($status === 400 && str_contains($reply['output'] ?? '', 'paste the whole pairing link'), 'a broken link is refused');
check(!file_exists("$root/args"), 'the command never ran for it');
check(str_contains(log_text(), 'isn\'t a whole pairing link'), 'the log says so');

// Emptying a trash folder runs the junk command with the rule's id only.
reset_root();
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'junk_clean', 'rule' => 'recycle-bin'], true);
check($status === 200 && ($reply['ok'] ?? false), 'emptying a trash folder works');
$args = explode("\n", trim((string)@file_get_contents("$root/args")));
check(array_slice($args, 0, 1) === ['unraid'] && array_slice($args, -3) === ['junk', 'clean', 'recycle-bin'], 'the command gets the rule id');
reset_root();
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'junk_clean', 'rule' => '../x'], true);
check($status === 400 && !file_exists("$root/args"), 'a rule id with other characters is refused before the command runs');

// A preset is asked for by kind; its names are checked before the command runs.
reset_root();
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'move_preset', 'kind' => 'empty-disk', 'disk' => 'disk1'], true);
check($status === 200, 'emptying a disk is planned');
$args = explode("\n", trim((string)@file_get_contents("$root/args")));
check($args[0] === 'unraid' && array_slice($args, -5) === ['move', 'preset', '--json', 'empty-disk', 'disk1'], 'the command gets the disk');
reset_root();
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'move_preset', 'kind' => 'gather', 'share' => 'Movies', 'to' => 'disk2'], true);
$args = explode("\n", trim((string)@file_get_contents("$root/args")));
check($status === 200 && array_slice($args, -5) === ['gather', '--to', 'disk2', '--', 'Movies'], 'gathering gets the share and the disk');
reset_root();
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'move_preset', 'kind' => 'gather', 'share' => '../x'], true);
check($status === 400 && !file_exists("$root/args"), 'a share with a slash is refused before the command runs');
reset_root();
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'move_start', 'share' => 'Movies', 'folder' => 'Old', 'from' => ['disk1'], 'to' => 'disk2', 'empty' => '1'], true);
$args = explode("\n", trim((string)@file_get_contents("$root/args")));
check($status === 200 && end($args) === '--empty-after', 'the tick to delete the originals reaches the command');

// Without Unraid's prelude the page checks the token itself.
reset_root();
[$status, $reply] = post(['action' => 'keep', 'paths' => ''], false);
check($status === 403 && ($reply['output'] ?? '') === 'the page is out of date; reload it', 'without the prelude, a missing token is refused');
[$status, $reply] = post(['csrf_token' => 'WRONG', 'action' => 'keep', 'paths' => ''], false);
check($status === 403, 'without the prelude, a wrong token is refused');
[$status, $reply] = post(['csrf_token' => 'T0KEN', 'action' => 'keep', 'paths' => ''], false);
check($status === 200 && ($reply['ok'] ?? false), 'without the prelude, the right token works');

exec('rm -rf '.escapeshellarg($root));
if ($failures) {
  echo "$failures failed\n";
  exit(1);
}
echo "all passed\n";
