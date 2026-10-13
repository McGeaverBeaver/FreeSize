<?php
// What Unraid's own prelude (emhttp/plugins/dynamix/include/local_prepend.php,
// run before every page script) does with a POST's token in Unraid 7.2 and
// 7.3: it stops the request when the token is wrong, and otherwise removes
// it from $_POST before the page's script runs.
function csrf_terminate($reason) {
  echo json_encode(['error' => "$reason csrf_token"]);
  exit;
}

$var = parse_ini_file(getenv('DISKTREE_TEST_ROOT').'/var/local/emhttp/var.ini');
if (!isset($var['csrf_token'])) csrf_terminate('uninitialized');
$csrf_token = $_POST['csrf_token'] ?? ($_SERVER['HTTP_X_CSRF_TOKEN'] ?? null);
if ($csrf_token === null) csrf_terminate('missing');
if (!hash_equals($var['csrf_token'], $csrf_token)) csrf_terminate('wrong');
unset($_POST['csrf_token']);
