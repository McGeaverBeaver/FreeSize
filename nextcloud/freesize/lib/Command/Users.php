<?php

declare(strict_types=1);

// SPDX-License-Identifier: AGPL-3.0-or-later

namespace OCA\FreeSize\Command;

use OCA\FreeSize\Service\Agent;
use OCP\IUserManager;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Input\InputOption;
use Symfony\Component\Console\Output\OutputInterface;

class Users extends Command {
	public function __construct(
		private Agent $agent,
		private IUserManager $users,
	) {
		parent::__construct();
	}

	protected function configure(): void {
		$this->setName('freesize:users')
			->setDescription('Show or change the Nextcloud users FreeSize covers')
			->addOption('add', null, InputOption::VALUE_REQUIRED | InputOption::VALUE_IS_ARRAY, 'Cover this user')
			->addOption('remove', null, InputOption::VALUE_REQUIRED | InputOption::VALUE_IS_ARRAY, 'Stop covering this user');
	}

	protected function execute(InputInterface $input, OutputInterface $output): int {
		$state = $this->agent->state();
		$covered = $state->users();
		foreach ($input->getOption('add') as $id) {
			if (!$this->users->userExists($id)) {
				$output->writeln("<error>No Nextcloud user \"$id\".</error>");
				return 1;
			}
			$covered[] = $id;
		}
		$covered = array_values(array_diff($covered, $input->getOption('remove')));
		$state->setUsers($covered);
		$covered = $state->users();
		$output->writeln($covered === [] ? 'No users covered yet.' : 'Covered: ' . implode(', ', $covered));
		return 0;
	}
}
