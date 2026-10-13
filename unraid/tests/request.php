<?php
// One request to the page's action.php, as Unraid's web server makes it.
// The test runner starts this once per request; action.php ends it.
$request = json_decode(getenv('DT_REQUEST'), true);
$_SERVER['REQUEST_METHOD'] = 'POST';
$_POST = $request['post'];
if (!empty($request['prelude'])) require __DIR__.'/unraid_prelude.php';
register_shutdown_function(function () {
  fwrite(STDERR, 'status '.http_response_code()."\n");
});
require getenv('DT_ACTION');
