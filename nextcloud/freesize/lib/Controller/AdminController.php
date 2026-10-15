<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Controller;

use OCA\FreeSize\AppInfo\Application;
use OCA\FreeSize\Hub\HubException;
use OCA\FreeSize\Service\Agent;
use OCP\AppFramework\Controller;
use OCP\AppFramework\Http;
use OCP\AppFramework\Http\Attribute\FrontpageRoute;
use OCP\AppFramework\Http\Attribute\PasswordConfirmationRequired;
use OCP\AppFramework\Http\JSONResponse;
use OCP\IAppConfig;
use OCP\IRequest;
use OCP\IUserManager;

/** The admin page's calls. Admins only: no attribute lets anyone else in. */
class AdminController extends Controller {
	public function __construct(
		IRequest $request,
		private Agent $agent,
		private IUserManager $users,
		private IAppConfig $appConfig,
	) {
		parent::__construct(Application::APP_ID, $request);
	}

	/** What the admin page shows. */
	public function snapshot(): array {
		$state = $this->agent->state();
		$approver = $state->approver();
		$covered = [];
		foreach ($state->users() as $id) {
			$user = $this->users->get($id);
			$covered[] = [
				'id' => $id,
				'name' => $user?->getDisplayName() ?? $id,
				'email' => (string)($user?->getEMailAddress() ?? ''),
				'missing' => $user === null,
			];
		}
		return [
			'paired' => $state->paired(),
			'hub' => $state->hub(),
			'hub_name' => $state->hubName(),
			'name' => $state->name(),
			'fingerprint' => $state->fingerprint(),
			'approver' => $approver === null ? null : ['name' => $approver['name'], 'fingerprint' => $state->approverFingerprint()],
			'grants' => count($state->grants(time())),
			'users' => $covered,
			'trash_bin' => $this->agent->trashBin(),
			'cron' => $this->appConfig->getValueString('core', 'backgroundjobs_mode', 'ajax'),
			'status' => $state->status(),
		];
	}

	private function answer(?string $error = null, ?string $said = null, ?string $fingerprint = null): JSONResponse {
		$body = ['state' => $this->snapshot()];
		if ($said !== null) {
			$body['said'] = $said;
		}
		if ($error !== null) {
			$body['error'] = $error;
			$body['hub_fingerprint'] = $fingerprint;
		}
		return new JSONResponse($body, $error === null ? Http::STATUS_OK : Http::STATUS_BAD_REQUEST);
	}

	#[FrontpageRoute(verb: 'GET', url: '/admin/state')]
	public function state(): JSONResponse {
		return $this->answer();
	}

	#[PasswordConfirmationRequired]
	#[FrontpageRoute(verb: 'POST', url: '/admin/pair')]
	public function pair(string $link = '', string $code = '', string $fingerprint = ''): JSONResponse {
		try {
			$name = $this->agent->pair($link, $code, $fingerprint === '' ? null : $fingerprint);
		} catch (HubException $e) {
			return $this->answer($e->getMessage(), null, $e->fingerprint);
		}
		$this->agent->run(true);
		return $this->answer(null, "Paired with $name.");
	}

	#[PasswordConfirmationRequired]
	#[FrontpageRoute(verb: 'POST', url: '/admin/unpair')]
	public function unpair(): JSONResponse {
		$this->agent->unpair();
		return $this->answer(null, 'Forgot the Hub. Remove this device from the dashboard too.');
	}

	#[PasswordConfirmationRequired]
	#[FrontpageRoute(verb: 'POST', url: '/admin/approver')]
	public function approver(string $fingerprint = ''): JSONResponse {
		try {
			$name = $this->agent->setApprover($fingerprint);
		} catch (HubException $e) {
			return $this->answer($e->getMessage());
		}
		return $this->answer(null, $name === null ? 'No approver: requests from the Hub are refused.' : "Requests are approved by $name now.");
	}

	#[FrontpageRoute(verb: 'POST', url: '/admin/grants/end')]
	public function endGrants(): JSONResponse {
		$ended = $this->agent->endGrants();
		return $this->answer(null, "Ended $ended grant(s).");
	}

	#[FrontpageRoute(verb: 'POST', url: '/admin/name')]
	public function rename(string $name = ''): JSONResponse {
		$this->agent->rename($name);
		return $this->answer(null, 'Saved.');
	}

	#[FrontpageRoute(verb: 'GET', url: '/admin/users')]
	public function searchUsers(string $search = ''): JSONResponse {
		$found = [];
		foreach ($this->users->searchDisplayName($search, 20) as $user) {
			$found[] = ['id' => $user->getUID(), 'name' => $user->getDisplayName(), 'email' => (string)($user->getEMailAddress() ?? '')];
		}
		return new JSONResponse(['users' => $found]);
	}

	#[FrontpageRoute(verb: 'POST', url: '/admin/users')]
	public function setUsers(string $add = '', string $remove = ''): JSONResponse {
		$state = $this->agent->state();
		$covered = $state->users();
		if ($add !== '') {
			if (!$this->users->userExists($add)) {
				return $this->answer("No Nextcloud user \"$add\".");
			}
			$covered[] = $add;
		}
		$state->setUsers(array_values(array_diff($covered, [$remove])));
		return $this->answer();
	}

	#[FrontpageRoute(verb: 'POST', url: '/admin/run')]
	public function run(): JSONResponse {
		$said = $this->agent->run(true);
		$error = $this->agent->state()->status()['last_error'];
		return $this->answer($error === '' ? null : $error, $said === '' ? 'Nothing new.' : $said);
	}
}
