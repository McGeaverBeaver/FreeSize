<?php
// FreeSize for Unraid: what the page's PHP shares. Everything that changes
// the server goes through the `freesize` command, which refuses what must
// never be touched; this file only passes arguments along, quoted.

$dt_root = getenv('FREESIZE_TEST_ROOT') ?: '';
$dt_bin = $dt_root ? getenv('FREESIZE_TEST_BIN') : '/usr/local/bin/freesize';
$dt_config = "$dt_root/boot/config/plugins/freesize";
$dt_data = "$dt_root/var/local/freesize";

/** A server path, inside the test folder when testing. */
function dt_file($path) {
  global $dt_root;
  return $dt_root.$path;
}

/**
 * Runs `freesize` with the arguments as given, each quoted. For `freesize
 * unraid`, where its folders are goes right after `unraid`, ahead of any
 * `--` that ends the options.
 */
function dt_run($args, $unraid = true) {
  global $dt_bin, $dt_root, $dt_config, $dt_data;
  if ($unraid) {
    $dirs = ['--config', $dt_config, '--data', $dt_data];
    if ($dt_root) array_push($dirs, '--root', $dt_root);
    array_splice($args, 1, 0, $dirs);
  }
  $parts = [escapeshellarg($dt_bin)];
  foreach ($args as $arg) $parts[] = escapeshellarg($arg);
  $out = [];
  $code = 1;
  exec(implode(' ', $parts).' </dev/null 2>&1', $out, $code);
  return ['ok' => $code === 0, 'output' => trim(implode("\n", $out))];
}

/** The Hub pairing file. */
function dt_state() {
  global $dt_config;
  return "$dt_config/agent.json";
}

/** Adds a line to the plugin's log, next to the background part's. */
function dt_log($line) {
  @file_put_contents(dt_file('/var/log/freesize.log'), $line."\n", FILE_APPEND);
}

/** Asks the background part for something. */
function dt_request($name) {
  global $dt_data;
  @mkdir("$dt_data/requests", 0755, true);
  return @file_put_contents("$dt_data/requests/$name", '') !== false;
}

/** The settings in freesize.cfg, with defaults. */
function dt_config() {
  global $dt_config;
  $cfg = @parse_ini_file("$dt_config/freesize.cfg") ?: [];
  return [
    'scan_hours' => (int)($cfg['SCAN_HOURS'] ?? 24),
    'wake_days' => (int)($cfg['WAKE_DAYS'] ?? 7),
    'permissions' => ($cfg['PERMISSIONS'] ?? 'yes') === 'yes',
    'report' => ($cfg['REPORT'] ?? 'yes') === 'yes',
    'keep' => trim((string)@file_get_contents("$dt_config/keep.txt")),
  ];
}

function dt_reply($body, $status = 200) {
  http_response_code($status);
  header('Content-Type: application/json');
  header('Cache-Control: no-store');
  echo json_encode($body);
  exit;
}

function dt_fail($message, $status = 400) {
  dt_reply(['ok' => false, 'output' => $message], $status);
}

/**
 * Unraid's own prelude (local_prepend.php, run before every page script)
 * checks the token on every POST, stops the request when it's wrong, and
 * then removes it from $_POST. So when the prelude ran there is nothing
 * left to compare, and the request already passed. Without the prelude
 * (the tests, or a web server set up differently) the token is checked here.
 */
function dt_check_csrf() {
  if (function_exists('csrf_terminate')) return;
  $var = @parse_ini_file(dt_file('/var/local/emhttp/var.ini')) ?: [];
  $token = $_POST['csrf_token'] ?? ($_SERVER['HTTP_X_CSRF_TOKEN'] ?? '');
  if (empty($var['csrf_token']) || !is_string($token) || !hash_equals($var['csrf_token'], $token)) {
    dt_fail('the page is out of date; reload it', 403);
  }
}

/** A list of server paths from the form, each on the array or a pool. */
function dt_paths($name) {
  $paths = $_POST[$name] ?? [];
  if (!is_array($paths) || !$paths || count($paths) > 500) dt_fail('choose 1 to 500 paths');
  $clean = [];
  foreach ($paths as $path) {
    $path = rtrim(trim((string)$path), '/');
    if (!preg_match('#^/mnt/[^\x00-\x1f]+$#', $path)) dt_fail("$path isn't on the array or a pool");
    $clean[] = $path;
  }
  return array_values(array_unique($clean));
}

/** A short, plain bit of text from the form. */
function dt_text($name, $max = 200) {
  $text = trim((string)($_POST[$name] ?? ''));
  $text = preg_replace('/[\x00-\x1f\x7f]/', ' ', $text);
  return function_exists('mb_substr') ? mb_substr($text, 0, $max) : substr($text, 0, $max);
}

/** An id the background part made: letters and digits. */
function dt_id($name) {
  $id = (string)($_POST[$name] ?? '');
  if (!preg_match('/^[A-Za-z0-9]{1,40}$/', $id)) dt_fail('no such item');
  return $id;
}
