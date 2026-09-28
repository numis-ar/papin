package ar.numis.papin.ui.nav

import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.DateRange
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import ar.numis.papin.di.AppContainer
import ar.numis.papin.ui.screens.AgentsScreen
import ar.numis.papin.ui.screens.ChatScreen
import ar.numis.papin.ui.screens.OnboardingScreen
import ar.numis.papin.ui.screens.SessionsScreen
import ar.numis.papin.ui.screens.SettingsScreen
import ar.numis.papin.ui.theme.PapinTheme

object Routes {
    const val ONBOARDING = "onboarding"
    const val AGENTS = "agents"
    const val SESSIONS = "sessions"
    const val SETTINGS = "settings"
    const val CHAT = "chat/{agentId}"
    fun chat(agentId: String) = "chat/$agentId"
}

/** Bottom nav: Agents / Sessions / Settings (PLAN §7). */
@Composable
fun AppRoot(
    container: AppContainer,
    startAgentId: String? = null,
    startSessionId: String? = null,
) {
    PapinTheme {
        val nav = rememberNavController()
        val start = when {
            !container.settings.onboarded() -> Routes.ONBOARDING
            startAgentId != null -> Routes.chat(startAgentId)
            else -> Routes.AGENTS
        }
        Scaffold(
            bottomBar = {
                val backStack by nav.currentBackStackEntryAsState()
                val route = backStack?.destination?.route
                if (route != Routes.ONBOARDING && route?.startsWith("chat/") != true) {
                    NavigationBar {
                        NavigationBarItem(
                            selected = route == Routes.AGENTS,
                            onClick = { nav.navigateTop(Routes.AGENTS) },
                            icon = { Icon(Icons.Default.Home, contentDescription = "Agents") },
                            label = { Text("Agents") },
                        )
                        NavigationBarItem(
                            selected = route == Routes.SESSIONS,
                            onClick = { nav.navigateTop(Routes.SESSIONS) },
                            icon = { Icon(Icons.Default.DateRange, contentDescription = "Sessions") },
                            label = { Text("Sessions") },
                        )
                        NavigationBarItem(
                            selected = route == Routes.SETTINGS,
                            onClick = { nav.navigateTop(Routes.SETTINGS) },
                            icon = { Icon(Icons.Default.Settings, contentDescription = "Settings") },
                            label = { Text("Settings") },
                        )
                    }
                }
            },
        ) { padding ->
            NavHost(
                navController = nav,
                startDestination = start,
                modifier = Modifier.padding(padding),
            ) {
                composable(Routes.ONBOARDING) {
                    OnboardingScreen(
                        container = container,
                        onDone = {
                            nav.navigate(Routes.AGENTS) {
                                popUpTo(Routes.ONBOARDING) { inclusive = true }
                            }
                        },
                    )
                }
                composable(Routes.AGENTS) {
                    AgentsScreen(
                        container = container,
                        onOpenChat = { agentId -> nav.navigate(Routes.chat(agentId)) },
                    )
                }
                composable(Routes.CHAT) { backStack ->
                    val agentId = backStack.arguments?.getString("agentId").orEmpty()
                    ChatScreen(
                        container = container,
                        agentId = agentId,
                        initialSessionId = startSessionId,
                        onBack = { nav.popBackStack() },
                    )
                }
                composable(Routes.SESSIONS) {
                    SessionsScreen(
                        container = container,
                        onOpenChat = { agentId -> nav.navigate(Routes.chat(agentId)) },
                    )
                }
                composable(Routes.SETTINGS) {
                    SettingsScreen(container = container)
                }
            }
        }
    }
}

private fun androidx.navigation.NavHostController.navigateTop(route: String) {
    navigate(route) {
        popUpTo(graph.findStartDestination().id) { saveState = true }
        launchSingleTop = true
        restoreState = true
    }
}
