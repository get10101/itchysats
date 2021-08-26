import { Box, Button, Center, Flex, HStack, Input, StackDivider, Text, VStack } from "@chakra-ui/react";
import React from "react";
import { Link as RouteLink, Route, Switch } from "react-router-dom";
import "./App.css";

function App() {
    return (
        <Center marginTop={50}>
            <HStack>
                <Box marginRight={5}>
                    <VStack align={"top"}>
                        <NavLink text={"trade"} path={"/trade"} />
                        <NavLink text={"wallet"} path={"/wallet"} />
                        <NavLink text={"settings"} path={"/settings"} />
                    </VStack>
                </Box>
                <Box width={1000} height={600}>
                    <Switch>
                        <Route path="/trade">
                            <Flex direction={"row"} height={"100%"}>
                                <Flex direction={"row"} width={"100%"}>
                                    <VStack
                                        spacing={5}
                                        shadow={"md"}
                                        padding={5}
                                        width={"100%"}
                                        divider={<StackDivider borderColor="gray.200" />}
                                    >
                                        <Box height={"70%"} width={"100%"} bg={"gray.100"} textAlign={"center"}>
                                            <Center>
                                                <Text>Graph</Text>
                                            </Center>
                                        </Box>
                                        <Box width={"100%"} overflow={"scroll"}>
                                            <Box>Make some data table happen here...</Box>
                                            <Box>Entry</Box>
                                            <Box>Entry</Box>
                                            <Box>Entry</Box>
                                            <Box>Entry</Box>
                                            <Box>Entry</Box>
                                            <Box>Entry</Box>
                                        </Box>
                                    </VStack>
                                </Flex>
                                <Flex width={"50%"} marginLeft={5}>
                                    <VStack spacing={5} shadow={"md"} padding={5} align={"stretch"}>
                                        <HStack>
                                            <Text align={"left"}>Current Price:</Text>
                                            <Text>490000</Text>
                                        </HStack>
                                        <HStack>
                                            <Text>Quantity:</Text>
                                            <Input></Input>
                                        </HStack>
                                        <Text>Leverage:</Text>
                                        <Button disabled={true}>x5</Button>
                                        <Button variant={"solid"} colorScheme={"blue"}>BUY</Button>
                                    </VStack>
                                </Flex>
                            </Flex>
                        </Route>
                        <Route path="/wallet">
                            <Center height={"100%"} shadow={"md"}>
                                <Box>
                                    <Text>Wallet</Text>
                                </Box>
                            </Center>
                        </Route>
                        <Route path="/settings">
                            <Center height={"100%"} shadow={"md"}>
                                <Box>
                                    <Text>Settings</Text>
                                </Box>
                            </Center>
                        </Route>
                    </Switch>
                </Box>
            </HStack>
        </Center>
    );
}

type NavLinkProps = { text: string; path: string };

const NavLink = ({ text, path }: NavLinkProps) => (
    <RouteLink to={path}>
        <Route
            path={path}
            children={({ match }) => (
                <Button width="100px" colorScheme="blue" variant={match?.path ? "solid" : "outline"}>
                    {text}
                </Button>
            )}
        />
    </RouteLink>
);

export default App;
