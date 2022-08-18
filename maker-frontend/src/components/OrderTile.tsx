import { Box, Grid, Text, VStack } from "@chakra-ui/react";
import React from "react";
import { MakerOffer } from "./Types";

interface MakerOfferProps {
    maker_offer: MakerOffer;
}

function OrderTile(
    {
        maker_offer,
    }: MakerOfferProps,
) {
    const labelWidth = 140;

    return (
        <Box borderRadius={"md"} borderColor={"blue.800"} borderWidth={2}>
            <VStack>
                <Box bg="blue.800" w="100%">
                    <Text padding={2} color={"white"} fontWeight={"bold"}>
                        {maker_offer.contract_symbol} {maker_offer.position} Offer
                    </Text>
                </Box>
                <Grid gridTemplateColumns="max-content auto" padding={5} rowGap={2}>
                    <Text width={labelWidth}>ID</Text>
                    <Text whiteSpace="nowrap">{maker_offer.id}</Text>

                    <Text width={labelWidth}>Symbol</Text>
                    <Text>{maker_offer.contract_symbol}</Text>

                    <Text width={labelWidth}>Price</Text>
                    <Text>{maker_offer.price}</Text>

                    <Text width={labelWidth}>Min Quantity</Text>
                    <Text>{maker_offer.min_quantity}</Text>

                    <Text width={labelWidth}>Max Quantity</Text>
                    <Text>{maker_offer.max_quantity}</Text>

                    <Text width={labelWidth}>Leverage Choices</Text>
                    <Text>[{maker_offer.leverage_details.map(leverage => `${leverage.leverage},`)}]</Text>

                    <Text width={labelWidth}>Opening Fee</Text>
                    <Text>{maker_offer.opening_fee}</Text>

                    <Text width={labelWidth}>Funding Rate</Text>
                    <Text>{maker_offer.funding_rate_hourly_percent}</Text>

                    <Text width={labelWidth}>Liquidation Price</Text>
                    <Text>[{maker_offer.leverage_details.map(leverage => `${leverage.liquidation_price},`)}]</Text>
                </Grid>
            </VStack>
        </Box>
    );
}

export default OrderTile;
